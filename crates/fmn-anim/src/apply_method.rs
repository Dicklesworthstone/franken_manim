//! Deferred native method targets, using Transform's existing lifecycle.
//!
//! A method is called on a fresh copy at **begin**, never at construction.
//! This is the Reference's `ApplyMethod.create_target` protocol and permits
//! operations outside the fixed `.animate` command vocabulary. The callback
//! stays on the scene owner thread; arbitrary effects are unclassified (R20).

use fmn_core::types::Vec3;
use fmn_mobject::{Mob, Stage};

use crate::animation::validate_begin_state;
use crate::{AnimConfig, AnimError, AnimState, Animation, PathFunc, Transform};

type Method = Box<dyn FnMut(&mut Stage, Mob) -> Result<(), AnimError>>;

/// Animate an arbitrary native operation on a begin-time copy of a mobject.
///
/// Unlike preparing an eager target with [`crate::apply_function`], a method
/// inside a [`crate::Succession`] sees the preceding member's final geometry.
/// Replaying the animation invokes the method again on a fresh live-source
/// copy. Its return value is only success/failure; the copy is always the target.
/// Alignment, records, point paths, tracker lanes, locks and cleanup are all
/// delegated to [`Transform`]. No callback is invoked during interpolation.
///
/// The callback may read other scene state or have external effects. It is
/// therefore deliberately unclassified, even if it happens to modify only its
/// target: such a segment is recorded rather than reconstructed by re-running
/// authored code on a worker. Use [`Self::with_dependencies`] to declare other
/// authored handles for preflight; this does not make the method pure.
///
/// ```
/// use fmn_anim::{Animation, ApplyMethod};
/// use fmn_mobject::{Mobject, Stage};
///
/// let mut stage = Stage::new();
/// let mob = stage.add(Mobject::from_points(&[[1.0, 0.0, 0.0]]));
/// let mut animation = ApplyMethod::new(mob, |stage, target| {
///     stage.shift(target, [2.0, 0.0, 0.0]);
///     Ok(())
/// });
/// stage.shift(mob, [4.0, 0.0, 0.0]); // Still included in the starting state.
/// animation.begin(&mut stage)?;
/// animation.finish(&mut stage);
/// assert_eq!(stage.get_center(mob), [7.0, 0.0, 0.0]);
/// # Ok::<(), fmn_anim::AnimError>(())
/// ```
pub struct ApplyMethod {
    transform: Transform,
    method: Method,
    path: PathFunc,
    dependencies: Vec<Mob>,
}

impl std::fmt::Debug for ApplyMethod {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ApplyMethod")
            .field("transform", &self.transform)
            .field("path", &self.path)
            .field("dependencies", &self.dependencies)
            .finish_non_exhaustive()
    }
}

impl ApplyMethod {
    /// Retain a method without calling it or copying any scene object.
    ///
    /// The method receives `(stage, target_copy)`. Captured mutable state is
    /// permitted, but is neither serialized nor rolled back if a later step
    /// fails. Fallible native operations can propagate [`AnimError`] with `?`.
    #[must_use]
    pub fn new(
        mobject: Mob,
        method: impl FnMut(&mut Stage, Mob) -> Result<(), AnimError> + 'static,
    ) -> Self {
        Self {
            transform: Transform::new(mobject, mobject).with_config(AnimConfig {
                name: "ApplyMethod".to_owned(),
                ..AnimConfig::default()
            }),
            method: Box::new(method),
            path: PathFunc::Straight,
            dependencies: Vec::new(),
        }
    }

    /// Map every world-space point of the begin-time family copy.
    ///
    /// This is the deferred `ApplyPointwiseFunction` form, with its native
    /// three-second default. The map uses Marionette's existing point-mapping
    /// operation about the true origin, including auxiliary pointlike fields.
    /// Changing the live family before begin is therefore included in the
    /// target. Non-point record fields keep Transform's normal interpolation.
    #[must_use]
    pub fn pointwise(mobject: Mob, map: impl Fn(Vec3) -> Vec3 + 'static) -> Self {
        Self::new(mobject, move |stage, target| {
            stage.apply_points_function(target, &map, Some([0.0; 3]), None);
            Ok(())
        })
        .with_config(AnimConfig {
            name: "ApplyPointwiseFunction".to_owned(),
            run_time: 3.0,
            ..AnimConfig::default()
        })
    }

    /// Move the begin-time copy's center to `map(center)` without deforming it.
    ///
    /// The center and destination are both evaluated at begin, after preceding
    /// succession members have finished. The map is called once per begin,
    /// not per member, interpolation sample or rendered frame.
    #[must_use]
    pub fn pointwise_to_center(mobject: Mob, mut map: impl FnMut(Vec3) -> Vec3 + 'static) -> Self {
        Self::new(mobject, move |stage, target| {
            let destination = map(stage.get_center(target));
            stage.move_to(target, destination, [0.0; 3]);
            Ok(())
        })
        .with_config(AnimConfig {
            name: "ApplyPointwiseFunctionToCenter".to_owned(),
            ..AnimConfig::default()
        })
    }

    /// Set ordinary animation arguments without evaluating the method.
    #[must_use]
    pub fn with_config(mut self, config: AnimConfig) -> Self {
        self.transform = self.transform.with_config(config);
        self
    }

    /// Use the existing native path law for all pointlike record fields.
    #[must_use]
    pub fn with_path_func(mut self, path: PathFunc) -> Self {
        self.path = path;
        self
    }

    /// The Transform `path_arc` / `path_arc_axis` convenience.
    #[must_use]
    pub fn with_path_arc(self, angle: f64, axis: Vec3) -> Self {
        self.with_path_func(PathFunc::from_path_arc(angle, axis))
    }

    /// Declare additional authored handles the method may read or publish.
    ///
    /// These participate in liveness and typesetting preflight, not zipped
    /// interpolation or animation-owned updater traversal. They must belong
    /// to the same Stage. Generated starting/target copies are never included.
    #[must_use]
    pub fn with_dependencies(mut self, dependencies: impl IntoIterator<Item = Mob>) -> Self {
        self.dependencies = dependencies.into_iter().collect();
        self
    }

    /// The aligned target used by the most recently successful begin.
    #[must_use]
    pub fn target_copy(&self) -> Option<Mob> {
        self.transform.target_copy()
    }
}

impl Animation for ApplyMethod {
    fn state(&self) -> &AnimState {
        self.transform.state()
    }

    fn state_mut(&mut self) -> &mut AnimState {
        self.transform.state_mut()
    }

    fn validate_begin(&self, stage: &Stage) -> Result<(), AnimError> {
        validate_begin_state(self.state(), stage, self.preflight_mobjects())?;
        self.path.validate()
    }

    fn setup(&mut self, stage: &mut Stage) -> Result<(), AnimError> {
        // Also protect callers using Animation::begin directly, rather than
        // through the driver's preflight. Authored code never runs for an
        // already-invalid handle, path or time span.
        self.validate_begin(stage)?;
        let source = self.state().mobject();
        let target = stage.copy_family(source)?;
        let private_members = stage.family(target);
        let config = self.state().config.clone();
        let mut transform = Transform::new(source, target)
            .with_config(config)
            .with_path_func(self.path);
        let result = (self.method)(stage, target).and_then(|()| transform.setup(stage));
        if let Err(error) = result {
            // Only our fresh copies are ours to reclaim. Do not delete a
            // pre-existing child attached by the callback, and do not undo
            // arbitrary authored side effects. The play driver's existing
            // begin transaction separately restores Stage state on failure.
            for member in private_members.into_iter().rev() {
                if stage.contains(member) {
                    let _ = stage.delete(member);
                }
            }
            return Err(error);
        }
        self.transform = transform;
        Ok(())
    }

    fn create_starting_mobject(&self, stage: &mut Stage) -> Result<Mob, AnimError> {
        self.transform.create_starting_mobject(stage)
    }

    fn after_begin(&mut self, stage: &mut Stage) {
        self.transform.after_begin(stage);
    }

    fn teardown(&mut self, stage: &mut Stage) {
        self.transform.teardown(stage);
    }

    fn clean_up_from_scene(&mut self, stage: &mut Stage) {
        self.transform.clean_up_from_scene(stage);
    }

    fn all_mobjects(&self) -> Vec<Mob> {
        self.transform.all_mobjects()
    }

    fn preflight_mobjects(&self) -> Vec<Mob> {
        let mut members = vec![self.state().mobject()];
        for &member in &self.dependencies {
            if !members.contains(&member) {
                members.push(member);
            }
        }
        members
    }

    fn interpolate_submobject(&mut self, stage: &mut Stage, mobs: &[Mob], sub_alpha: f64) {
        self.transform
            .interpolate_submobject(stage, mobs, sub_alpha);
    }
    // Keep Animation's Unclassified effect signature. The Transform delegate
    // is pure, but that says nothing about the authored target factory.
}
