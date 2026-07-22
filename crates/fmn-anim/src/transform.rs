//! The Transform mechanism — family 1 of the five (§9.4, fm-cye):
//! alignment + field lerp through a path function, ported from the pinned
//! Reference (transform.py, utils/paths.py, mobject.py:1810).
//!
//! - [`PathFunc`] carries the exact path formulas: `straight_path` is the
//!   plain lerp; `path_along_arc` rotates the start radius about the
//!   computed arc center (`center + cos(αθ)·r + sin(αθ)·(axis×r)`), and a
//!   scalar `|arc_angle| <` [`STRAIGHT_PATH_THRESHOLD`] collapses to
//!   straight, exactly as the Reference's early return.
//! - The lerp core routes **only pointlike record fields through the path
//!   function**; every other field lerps linearly, locked fields are
//!   skipped entirely, and numeric uniforms lerp linearly
//!   (mobject.py:1810). The Reference's `const_data_keys` broadcast is a
//!   pure optimization (a constant column lerps to the same values
//!   row-wise) and is deliberately not replicated.
//! - [`Transform::setup`] aligns (`align_data_and_family`) before the
//!   starting copy is taken; `after_begin` locks matching data (skipped
//!   when the family has updaters), `teardown` unlocks — the Reference's
//!   begin/finish order, hook for hook.
//!
//! Zoo members here are parameterizations, never re-implementations:
//! `ReplacementTransform` flips the replace-in-scene flag (consumed by the
//! scene runtime, fm-5xm), `TransformFromCopy` animates a fresh copy,
//! `MoveToTarget`/`Restore` consume the §8.3 target/saved-state links, and
//! the `ApplyMethod` family builds targets by applying stage operations to
//! a copy. The remaining catalog (partial-reveal, fade/grow, indication,
//! functional maps) lands in later fm-cye slices.

use fmn_core::types::Vec3;
use fmn_mobject::{Mob, Stage};

use crate::animation::{AnimConfig, AnimError, AnimState, Animation, AnimationSignature};

/// `STRAIGHT_PATH_THRESHOLD` (utils/paths.py): scalar arc angles below
/// this collapse to the straight path.
pub const STRAIGHT_PATH_THRESHOLD: f64 = 0.01;

fn cross(a: Vec3, b: Vec3) -> Vec3 {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}

fn norm(v: Vec3) -> f64 {
    (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt()
}

/// NumPy `nan_to_num`: NaN → 0, ±∞ → ±`f64::MAX` — the Reference guards
/// the arc-center division with it.
fn nan_to_num(v: f64) -> f64 {
    if v.is_nan() {
        0.0
    } else if v == f64::INFINITY {
        f64::MAX
    } else if v == f64::NEG_INFINITY {
        f64::MIN
    } else {
        v
    }
}

/// A path function as composable data (journal-able, like `RateFunc`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum PathFunc {
    /// `straight_path`: `(1-α)·start + α·end`.
    Straight,
    /// `path_along_arc(arc_angle, axis)`.
    Arc {
        /// Signed arc angle in radians.
        angle: f64,
        /// Rotation axis (unit-normalized at eval; zero norm → OUT).
        axis: Vec3,
    },
}

impl PathFunc {
    /// The Reference's `init_path_func` rule: `path_arc == 0` (below the
    /// threshold) is the straight path, anything else arcs about `axis`.
    #[must_use]
    pub fn from_path_arc(angle: f64, axis: Vec3) -> Self {
        if angle.abs() < STRAIGHT_PATH_THRESHOLD {
            Self::Straight
        } else {
            Self::Arc { angle, axis }
        }
    }

    /// Evaluate the path point-for-point (utils/paths.py exactly).
    #[must_use]
    pub fn eval(&self, start: Vec3, end: Vec3, alpha: f64) -> Vec3 {
        match *self {
            Self::Straight => [
                (1.0 - alpha) * start[0] + alpha * end[0],
                (1.0 - alpha) * start[1] + alpha * end[1],
                (1.0 - alpha) * start[2] + alpha * end[2],
            ],
            Self::Arc { angle, axis } => {
                if angle.abs() < STRAIGHT_PATH_THRESHOLD {
                    return Self::Straight.eval(start, end, alpha);
                }
                let n = norm(axis);
                let unit: Vec3 = if n == 0.0 {
                    [0.0, 0.0, 1.0] // OUT
                } else {
                    [axis[0] / n, axis[1] / n, axis[2] / n]
                };
                let half: Vec3 = [
                    (end[0] - start[0]) / 2.0,
                    (end[1] - start[1]) / 2.0,
                    (end[2] - start[2]) / 2.0,
                ];
                let tan_half = fmn_dmath::tan(angle / 2.0);
                let c = cross(unit, half);
                let adjustment: Vec3 = [
                    nan_to_num(c[0] / tan_half),
                    nan_to_num(c[1] / tan_half),
                    nan_to_num(c[2] / tan_half),
                ];
                let center: Vec3 = [
                    start[0] + half[0] + adjustment[0],
                    start[1] + half[1] + adjustment[1],
                    start[2] + half[2] + adjustment[2],
                ];
                let c_to_start: Vec3 = [
                    start[0] - center[0],
                    start[1] - center[1],
                    start[2] - center[2],
                ];
                let c_to_perp = cross(unit, c_to_start);
                let (sin_a, cos_a) = (fmn_dmath::sin(alpha * angle), fmn_dmath::cos(alpha * angle));
                [
                    center[0] + cos_a * c_to_start[0] + sin_a * c_to_perp[0],
                    center[1] + cos_a * c_to_start[1] + sin_a * c_to_perp[1],
                    center[2] + cos_a * c_to_start[2] + sin_a * c_to_perp[2],
                ]
            }
        }
    }
}

// ------------------------------------------------------------- lerp core

/// The Reference's `Mobject.interpolate` (mobject.py:1810) over one
/// zipped submobject triple: pointlike fields route through `path`, every
/// other field lerps linearly, locked fields are skipped, numeric
/// uniforms lerp linearly. Computed in f64, stored at record precision.
pub fn interpolate_fields(
    stage: &mut Stage,
    submob: Mob,
    from: Mob,
    to: Mob,
    alpha: f64,
    path: PathFunc,
) {
    let Some(entry) = stage.get(submob) else {
        return;
    };
    let schema = entry.buffer.schema();
    let pointlike: Vec<String> = schema.pointlike_keys().to_vec();
    let fields: Vec<String> = schema
        .fields()
        .iter()
        .map(|f| f.name.clone())
        .filter(|name| !entry.buffer.is_locked(name))
        .collect();
    for field in fields {
        let (Some(a), Some(b)) = (
            stage.get(from).and_then(|e| e.buffer.read_column(&field)),
            stage.get(to).and_then(|e| e.buffer.read_column(&field)),
        ) else {
            continue;
        };
        if a.len() != b.len() {
            continue; // alignment holds by construction; a foreign write mid-play is skipped, not garbled
        }
        #[allow(clippy::cast_possible_truncation)]
        let out: Vec<f32> = if pointlike.contains(&field) {
            let (pa, ra) = a.as_chunks::<3>();
            let (pb, _) = b.as_chunks::<3>();
            debug_assert!(ra.is_empty(), "pointlike fields are 3-lane");
            pa.iter()
                .zip(pb.iter())
                .flat_map(|(ca, cb)| {
                    let p = path.eval(
                        [f64::from(ca[0]), f64::from(ca[1]), f64::from(ca[2])],
                        [f64::from(cb[0]), f64::from(cb[1]), f64::from(cb[2])],
                        alpha,
                    );
                    [p[0] as f32, p[1] as f32, p[2] as f32]
                })
                .collect()
        } else {
            a.iter()
                .zip(b.iter())
                .map(|(&x, &y)| ((1.0 - alpha) * f64::from(x) + alpha * f64::from(y)) as f32)
                .collect()
        };
        if let Some(entry) = stage.get_mut(submob) {
            entry.buffer.write_range(&field, 0, &out);
        }
    }
    // Numeric uniforms lerp linearly (the Reference lerps every shared
    // uniform key); discrete state (flags, joint type) stays the live
    // mobject's own.
    let (Some(ua), Some(ub)) = (
        stage.get(from).map(|e| *e.uniforms()),
        stage.get(to).map(|e| *e.uniforms()),
    ) else {
        return;
    };
    if let Some(entry) = stage.get_mut(submob) {
        let u = entry.uniforms_mut();
        let lerp = |x: f64, y: f64| (1.0 - alpha) * x + alpha * y;
        u.is_fixed_in_frame = lerp(ua.is_fixed_in_frame, ub.is_fixed_in_frame);
        u.anti_alias_width = lerp(ua.anti_alias_width, ub.anti_alias_width);
        for k in 0..3 {
            u.shading[k] = lerp(ua.shading[k], ub.shading[k]);
        }
        for (plane, (pa, pb)) in u
            .clip_planes
            .iter_mut()
            .zip(ua.clip_planes.iter().zip(ub.clip_planes.iter()))
        {
            for (slot, (&x, &y)) in plane.iter_mut().zip(pa.iter().zip(pb.iter())) {
                *slot = lerp(x, y);
            }
        }
    }
}

/// Reference `lock_matching_data` (mobject.py:1871): over the zipped
/// families, lock every record field whose columns are identical in the
/// start and target — those never need interpolating. No-op when the
/// animated family has updaters, exactly as the Reference.
fn lock_matching_data(stage: &mut Stage, mobject: Mob, m1: Mob, m2: Mob) {
    if stage.has_updaters_in_family(mobject) {
        return;
    }
    let family = stage.family(mobject);
    let f1 = stage.family(m1);
    let f2 = stage.family(m2);
    for ((&sub, &a), &b) in family.iter().zip(f1.iter()).zip(f2.iter()) {
        let (Some(ea), Some(eb)) = (stage.get(a), stage.get(b)) else {
            continue;
        };
        if ea.buffer.schema() != eb.buffer.schema() {
            continue; // Reference: skip on dtype mismatch
        }
        let matching: Vec<String> = ea
            .buffer
            .schema()
            .fields()
            .iter()
            .map(|f| f.name.clone())
            .filter(|name| ea.buffer.read_column(name) == eb.buffer.read_column(name))
            .collect();
        if let Some(entry) = stage.get_mut(sub) {
            entry.buffer.lock_data(matching.iter().map(String::as_str));
        }
    }
}

fn unlock_family_data(stage: &mut Stage, mobject: Mob) {
    for member in stage.family(mobject) {
        if let Some(entry) = stage.get_mut(member) {
            entry.buffer.unlock_data();
        }
    }
}

// -------------------------------------------------------------- Transform

/// The Transform animation (transform.py:24): align, then lerp fields
/// from the starting copy to an aligned target copy through the path
/// function.
#[derive(Debug, Clone)]
pub struct Transform {
    state: AnimState,
    target: Mob,
    target_copy: Option<Mob>,
    path: PathFunc,
    replace_in_scene: bool,
}

impl Transform {
    /// `Transform(mobject, target_mobject)` with the straight path.
    #[must_use]
    pub fn new(mobject: Mob, target: Mob) -> Self {
        let config = AnimConfig {
            name: "Transform".to_owned(),
            ..AnimConfig::default()
        };
        Self {
            state: AnimState::new(mobject, config),
            target,
            target_copy: None,
            path: PathFunc::Straight,
            replace_in_scene: false,
        }
    }

    /// `path_arc` / `path_arc_axis` (the Reference's `init_path_func`).
    #[must_use]
    pub fn with_path_arc(mut self, angle: f64, axis: Vec3) -> Self {
        self.path = PathFunc::from_path_arc(angle, axis);
        self
    }

    /// Explicit `path_func` override.
    #[must_use]
    pub fn with_path_func(mut self, path: PathFunc) -> Self {
        self.path = path;
        self
    }

    /// Replace the animation config (run_time, rate_func, lag_ratio …).
    #[must_use]
    pub fn with_config(mut self, config: AnimConfig) -> Self {
        self.state.config = config;
        self
    }

    fn with_replace_flag(mut self) -> Self {
        self.replace_in_scene = true;
        self
    }

    /// The Reference's `replace_mobject_with_target_in_scene` flag — the
    /// scene runtime (fm-5xm) consumes it in `clean_up_from_scene`.
    #[must_use]
    pub fn replaces_mobject_in_scene(&self) -> bool {
        self.replace_in_scene
    }

    /// The aligned copy actually interpolated toward (test hook).
    #[must_use]
    pub fn target_copy(&self) -> Option<Mob> {
        self.target_copy
    }
}

impl Animation for Transform {
    fn state(&self) -> &AnimState {
        &self.state
    }

    fn state_mut(&mut self) -> &mut AnimState {
        &mut self.state
    }

    /// Pure: interpolation writes records as a function of the frozen
    /// start/target pair and alpha.
    fn effect_signature(&self) -> AnimationSignature {
        AnimationSignature::Pure
    }

    /// transform.py:54 — target copy (shared when already aligned), then
    /// `align_data_and_family`, all before the starting copy is taken.
    fn setup(&mut self, stage: &mut Stage) -> Result<(), AnimError> {
        let mobject = self.state.mobject();
        if !stage.contains(mobject) {
            return Err(AnimError::StaleHandle(mobject));
        }
        if !stage.contains(self.target) {
            return Err(AnimError::StaleHandle(self.target));
        }
        let target_copy = if stage.is_aligned_with(mobject, self.target) {
            self.target
        } else {
            stage.copy_family(self.target)?
        };
        stage.align_data_and_family(mobject, target_copy)?;
        self.target_copy = Some(target_copy);
        Ok(())
    }

    /// transform.py:69 — lock matching data after `super().begin()`.
    fn after_begin(&mut self, stage: &mut Stage) {
        let mobject = self.state.mobject();
        if let (Some(starting), Some(target_copy)) =
            (self.state.starting_mobject(), self.target_copy)
        {
            lock_matching_data(stage, mobject, starting, target_copy);
        }
    }

    /// transform.py:74 — unlock in `finish`.
    fn teardown(&mut self, stage: &mut Stage) {
        unlock_family_data(stage, self.state.mobject());
    }

    /// `(mobject, starting_mobject, target_copy)` — transform.py:111.
    fn all_mobjects(&self) -> Vec<Mob> {
        let mut mobs = vec![self.state.mobject()];
        if let Some(starting) = self.state.starting_mobject() {
            mobs.push(starting);
        }
        if let Some(target_copy) = self.target_copy {
            mobs.push(target_copy);
        }
        mobs
    }

    fn interpolate_submobject(&mut self, stage: &mut Stage, mobs: &[Mob], sub_alpha: f64) {
        let [submob, starting, target] = *mobs else {
            return; // rows are triples once begin has run
        };
        interpolate_fields(stage, submob, starting, target, sub_alpha, self.path);
    }
}

// ------------------------------------------------------------------- zoo

/// `ReplacementTransform` (transform.py:132): a Transform whose clean-up
/// swaps the mobject for the target in the scene.
#[must_use]
pub fn replacement_transform(mobject: Mob, target: Mob) -> Transform {
    let mut t = Transform::new(mobject, target).with_replace_flag();
    t.state.config.name = "ReplacementTransform".to_owned();
    t
}

/// `TransformFromCopy` (transform.py:136): animate a fresh copy of the
/// source onto the target, replacing in scene.
///
/// # Errors
/// [`AnimError::StaleHandle`] / [`AnimError::Stage`].
pub fn transform_from_copy(
    stage: &mut Stage,
    mobject: Mob,
    target: Mob,
) -> Result<Transform, AnimError> {
    let copy = stage.copy_family(mobject)?;
    let mut t = Transform::new(copy, target).with_replace_flag();
    t.state.config.name = "TransformFromCopy".to_owned();
    Ok(t)
}

/// `MoveToTarget` (transform.py:143): transform onto the §8.3
/// `generate_target` copy.
///
/// # Errors
/// [`AnimError::MissingTarget`] without a generated target.
pub fn move_to_target(stage: &Stage, mobject: Mob) -> Result<Transform, AnimError> {
    let target = stage.target(mobject).ok_or(AnimError::MissingTarget)?;
    let mut t = Transform::new(mobject, target);
    t.state.config.name = "MoveToTarget".to_owned();
    Ok(t)
}

/// `Restore` (transform.py:248): transform back onto the §8.3
/// `save_state` copy.
///
/// # Errors
/// [`AnimError::MissingSavedState`] without a saved state.
pub fn restore(stage: &Stage, mobject: Mob) -> Result<Transform, AnimError> {
    let saved = stage
        .saved_state(mobject)
        .ok_or(AnimError::MissingSavedState)?;
    let mut t = Transform::new(mobject, saved);
    t.state.config.name = "Restore".to_owned();
    Ok(t)
}

/// The `ApplyFunction` mechanism (transform.py:255): the target is a copy
/// of the mobject with `f` applied — `ApplyMethod` and its whole
/// parameterization family reduce to this under the arena (a bound method
/// call is a stage operation on the copy).
///
/// # Errors
/// [`AnimError::StaleHandle`] / [`AnimError::Stage`].
pub fn apply_function(
    stage: &mut Stage,
    mobject: Mob,
    f: impl FnOnce(&mut Stage, Mob),
) -> Result<Transform, AnimError> {
    let target = stage.copy_family(mobject)?;
    f(stage, target);
    let mut t = Transform::new(mobject, target);
    t.state.config.name = "ApplyFunction".to_owned();
    Ok(t)
}

/// `ScaleInPlace` (transform.py:233): `ApplyMethod(mobject.scale, k)`.
///
/// # Errors
/// As [`apply_function`].
pub fn scale_in_place(
    stage: &mut Stage,
    mobject: Mob,
    scale_factor: f64,
) -> Result<Transform, AnimError> {
    let mut t = apply_function(stage, mobject, |s, m| {
        s.scale(m, scale_factor);
    })?;
    t.state.config.name = "ScaleInPlace".to_owned();
    Ok(t)
}

/// `ShrinkToCenter` (transform.py:243): `ScaleInPlace(mobject, 0)`.
///
/// # Errors
/// As [`apply_function`].
pub fn shrink_to_center(stage: &mut Stage, mobject: Mob) -> Result<Transform, AnimError> {
    let mut t = scale_in_place(stage, mobject, 0.0)?;
    t.state.config.name = "ShrinkToCenter".to_owned();
    Ok(t)
}

/// `CyclicReplace` (transform.py:316): each mobject transforms (along a
/// 90° arc by default) onto the position of its cyclic predecessor's
/// slot: targets are copies moved to `mobjects[(i+1) % n]`'s center.
/// Returns one Transform per mobject (the composition bead, fm-hfe, wraps
/// them in a group).
///
/// # Errors
/// [`AnimError::StaleHandle`] / [`AnimError::Stage`].
pub fn cyclic_replace(
    stage: &mut Stage,
    mobjects: &[Mob],
    path_arc: f64,
) -> Result<Vec<Transform>, AnimError> {
    let centers: Vec<Vec3> = mobjects.iter().map(|&m| stage.get_center(m)).collect();
    let n = mobjects.len();
    let mut out = Vec::with_capacity(n);
    for (i, &mob) in mobjects.iter().enumerate() {
        let dest = centers[(i + 1) % n];
        let mut t = apply_function(stage, mob, |s, m| {
            let center = s.get_center(m);
            s.shift(
                m,
                [
                    dest[0] - center[0],
                    dest[1] - center[1],
                    dest[2] - center[2],
                ],
            );
        })?
        .with_path_arc(path_arc, [0.0, 0.0, 1.0]);
        t.state.config.name = "CyclicReplace".to_owned();
        out.push(t);
    }
    Ok(out)
}

/// `Swap` (transform.py:329): `CyclicReplace` under its common two-element
/// name, same 90° default arc.
///
/// # Errors
/// As [`cyclic_replace`].
pub fn swap(stage: &mut Stage, a: Mob, b: Mob) -> Result<Vec<Transform>, AnimError> {
    let mut out = cyclic_replace(stage, &[a, b], std::f64::consts::FRAC_PI_2)?;
    for t in &mut out {
        t.state.config.name = "Swap".to_owned();
    }
    Ok(out)
}
