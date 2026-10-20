//! Update-driven animations, ported from the pinned Reference's
//! `animation/update.py`: closures run against the live stage each frame.
//! All three are inherently effectful (arbitrary user code, or reads of
//! concurrently animated state), so they keep the Unclassified signature
//! and the R20 demotion — their segments never frame-parallelize.
//!
//! `numbers.py`'s `ChangingDecimal`/`ChangeDecimalToValue`/`CountInFrom`
//! use this same mechanism, including its fallible callback boundary.

use fmn_core::types::Vec3;
use fmn_mobject::{Mob, Stage};

use crate::animation::{AnimConfig, AnimError, AnimState, Animation, time_spanned_alpha};

/// The boxed update closure: `(stage, mobject, true_alpha)`.
type UpdateClosure = Box<dyn FnMut(&mut Stage, Mob, f64) -> Result<(), AnimError>>;

/// `UpdateFromFunc` (update.py:13) and, alpha-aware,
/// `UpdateFromAlphaFunc` (update.py:37): run a stage closure against the
/// mobject each frame.
pub struct UpdateFromFunc {
    state: AnimState,
    update: UpdateClosure,
    alpha_aware: bool,
    error: Option<AnimError>,
}

impl std::fmt::Debug for UpdateFromFunc {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("UpdateFromFunc")
            .field("state", &self.state)
            .field("alpha_aware", &self.alpha_aware)
            .field("error", &self.error)
            .finish_non_exhaustive()
    }
}

impl UpdateFromFunc {
    /// `UpdateFromFunc(mobject, update_function)`.
    #[must_use]
    pub fn new(mobject: Mob, update: impl FnMut(&mut Stage, Mob) + 'static) -> Self {
        let mut update = update;
        Self::try_new(mobject, move |stage, mob| {
            update(stage, mob);
            Ok(())
        })
    }

    /// `UpdateFromAlphaFunc(mobject, update_function)`: the closure also
    /// receives `rate(time_spanned(α))` (update.py:50's `true_alpha`).
    #[must_use]
    pub fn new_alpha(mobject: Mob, update: impl FnMut(&mut Stage, Mob, f64) + 'static) -> Self {
        let mut update = update;
        Self::try_new_alpha(mobject, move |stage, mob, alpha| {
            update(stage, mob, alpha);
            Ok(())
        })
    }

    /// An update callback that can refuse its sample without panicking.
    ///
    /// The first error is retained by [`Animation::deferred_error`]. No
    /// further samples call the callback until a new `begin`. This does not
    /// roll back arbitrary callback side effects; prepare fallible work before
    /// installing it into the stage. The callback stays on the scene owner.
    #[must_use]
    pub fn try_new(
        mobject: Mob,
        update: impl FnMut(&mut Stage, Mob) -> Result<(), AnimError> + 'static,
    ) -> Self {
        let mut update = update;
        Self {
            state: AnimState::new(
                mobject,
                AnimConfig {
                    name: "UpdateFromFunc".to_owned(),
                    ..AnimConfig::default()
                },
            ),
            update: Box::new(move |stage, mob, _| update(stage, mob)),
            alpha_aware: false,
            error: None,
        }
    }

    /// Alpha-aware fallible update, using the same time-span and rate pipeline
    /// as [`Self::new_alpha`]. Errors follow [`Self::try_new`]'s first-failure
    /// rule; this remains effectful and is never submitted to pure workers.
    #[must_use]
    pub fn try_new_alpha(
        mobject: Mob,
        update: impl FnMut(&mut Stage, Mob, f64) -> Result<(), AnimError> + 'static,
    ) -> Self {
        Self {
            state: AnimState::new(
                mobject,
                AnimConfig {
                    name: "UpdateFromAlphaFunc".to_owned(),
                    ..AnimConfig::default()
                },
            ),
            update: Box::new(update),
            alpha_aware: true,
            error: None,
        }
    }

    /// Replace the animation config.
    #[must_use]
    pub fn with_config(mut self, config: AnimConfig) -> Self {
        self.state.config = config;
        self
    }
}

impl Animation for UpdateFromFunc {
    fn state(&self) -> &AnimState {
        &self.state
    }

    fn state_mut(&mut self) -> &mut AnimState {
        &mut self.state
    }

    fn setup(&mut self, stage: &mut Stage) -> Result<(), AnimError> {
        let mobject = self.state.mobject();
        if !stage.contains(mobject) {
            return Err(AnimError::StaleHandle(mobject));
        }
        self.error = None;
        Ok(())
    }

    /// update.py:33/49: the plain variant ignores alpha entirely; the
    /// alpha variant passes `rate(time_spanned(α))`. Lag never applies.
    fn interpolate(&mut self, stage: &mut Stage, alpha: f64) {
        if self.error.is_some() {
            return;
        }
        let true_alpha = if self.alpha_aware {
            let config = &self.state.config;
            config
                .rate_func
                .eval(time_spanned_alpha(alpha, config.run_time, config.time_span))
        } else {
            alpha
        };
        self.error = (self.update)(stage, self.state.mobject(), true_alpha).err();
    }

    fn deferred_error(&self) -> Option<AnimError> {
        self.error.clone()
    }

    fn interpolate_submobject(&mut self, _stage: &mut Stage, _mobs: &[Mob], _sub_alpha: f64) {
        // Unreachable: interpolate is overridden and never zips families.
    }
}

/// `MaintainPositionRelativeTo` (update.py:55): hold the
/// construction-time center offset against a tracked mobject.
#[derive(Debug, Clone)]
pub struct MaintainPositionRelativeTo {
    state: AnimState,
    tracked: Mob,
    diff: Vec3,
}

impl MaintainPositionRelativeTo {
    /// Captures `mobject.get_center() − tracked.get_center()` now, holds
    /// it each frame.
    #[must_use]
    pub fn new(stage: &Stage, mobject: Mob, tracked: Mob) -> Self {
        let a = stage.get_center(mobject);
        let b = stage.get_center(tracked);
        Self {
            state: AnimState::new(
                mobject,
                AnimConfig {
                    name: "MaintainPositionRelativeTo".to_owned(),
                    ..AnimConfig::default()
                },
            ),
            tracked,
            diff: [a[0] - b[0], a[1] - b[1], a[2] - b[2]],
        }
    }

    /// Replace the animation config.
    #[must_use]
    pub fn with_config(mut self, config: AnimConfig) -> Self {
        self.state.config = config;
        self
    }
}

impl Animation for MaintainPositionRelativeTo {
    fn state(&self) -> &AnimState {
        &self.state
    }

    fn state_mut(&mut self) -> &mut AnimState {
        &mut self.state
    }

    fn setup(&mut self, stage: &mut Stage) -> Result<(), AnimError> {
        for mob in [self.state.mobject(), self.tracked] {
            if !stage.contains(mob) {
                return Err(AnimError::StaleHandle(mob));
            }
        }
        Ok(())
    }

    fn preflight_mobjects(&self) -> Vec<Mob> {
        vec![self.state.mobject(), self.tracked]
    }

    /// update.py:66: shift so the tracked offset holds. Reads the tracked
    /// mobject *live* — which is the point, and why it stays Unclassified.
    fn interpolate(&mut self, stage: &mut Stage, _alpha: f64) {
        let target = stage.get_center(self.tracked);
        let location = stage.get_center(self.state.mobject());
        stage.shift(
            self.state.mobject(),
            [
                target[0] - location[0] + self.diff[0],
                target[1] - location[1] + self.diff[1],
                target[2] - location[2] + self.diff[2],
            ],
        );
    }

    fn interpolate_submobject(&mut self, _stage: &mut Stage, _mobs: &[Mob], _sub_alpha: f64) {
        // Unreachable: interpolate is overridden and never zips families.
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::animation::{AnimationSignature, RateFunc};
    use fmn_mobject::Mobject;
    use std::cell::{Cell, RefCell};
    use std::rc::Rc;

    #[test]
    fn fallible_update_retains_first_failure_and_stops_callback_side_effects() {
        let mut stage = Stage::new();
        let mob = stage.add(Mobject::new());
        let calls = Rc::new(Cell::new(0));
        let observed = Rc::clone(&calls);
        let expected = AnimError::UpdateFailed {
            operation: "fixture",
            detail: "layout refused".to_owned(),
        };
        let failure = expected.clone();
        let mut animation = UpdateFromFunc::try_new_alpha(mob, move |_, _, alpha| {
            observed.set(observed.get() + 1);
            if alpha >= 0.5 {
                Err(failure.clone())
            } else {
                Ok(())
            }
        })
        .with_config(AnimConfig {
            rate_func: RateFunc::linear(),
            ..AnimConfig::default()
        });
        animation.begin(&mut stage).unwrap();
        animation.interpolate(&mut stage, 0.25);
        assert!(animation.deferred_error().is_none());
        animation.interpolate(&mut stage, 0.5);
        assert_eq!(animation.deferred_error(), Some(expected.clone()));
        let at_failure = calls.get();
        animation.interpolate(&mut stage, 0.75);
        animation.finish(&mut stage);
        assert_eq!(calls.get(), at_failure);
        assert_eq!(animation.deferred_error(), Some(expected));
        assert_eq!(
            animation.effect_signature(),
            AnimationSignature::Unclassified
        );
        assert!(animation.freeze_interpolation().is_none());
        animation.begin(&mut stage).unwrap();
        assert!(animation.deferred_error().is_none());
        assert_eq!(calls.get(), at_failure + 1);
        animation.abort(&mut stage);
    }

    #[test]
    fn fallible_alpha_applies_time_span_and_rate_exactly_once() {
        let mut stage = Stage::new();
        let mob = stage.add(Mobject::new());
        let samples = Rc::new(RefCell::new(Vec::new()));
        let observed = Rc::clone(&samples);
        let mut animation = UpdateFromFunc::try_new_alpha(mob, move |_, _, alpha| {
            observed.borrow_mut().push(alpha);
            Ok(())
        })
        .with_config(AnimConfig {
            run_time: 4.0,
            time_span: Some((1.0, 3.0)),
            rate_func: RateFunc::linear().not_quite_there(0.5),
            lag_ratio: 1.0,
            ..AnimConfig::default()
        });
        animation.begin(&mut stage).unwrap();
        for alpha in [0.25, 0.5, 0.75, 1.0] {
            animation.interpolate(&mut stage, alpha);
        }
        assert_eq!(*samples.borrow(), [0.0, 0.0, 0.25, 0.5, 0.5]);
        assert!(animation.deferred_error().is_none());
    }

    #[test]
    fn fallible_plain_update_has_the_existing_begin_callback_count() {
        let mut stage = Stage::new();
        let mob = stage.add(Mobject::new());
        let calls = Rc::new(Cell::new(0));
        let observed = Rc::clone(&calls);
        let mut animation = UpdateFromFunc::try_new(mob, move |_, target| {
            assert_eq!(target, mob);
            observed.set(observed.get() + 1);
            Err(AnimError::EmptyMobject)
        });
        animation.begin(&mut stage).unwrap();
        assert_eq!(calls.get(), 1);
        assert_eq!(animation.deferred_error(), Some(AnimError::EmptyMobject));
        animation.interpolate(&mut stage, 1.0);
        assert_eq!(calls.get(), 1);
        animation.abort(&mut stage);
    }
}
