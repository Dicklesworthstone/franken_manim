//! Rotation animations, ported from the pinned Reference's
//! `animation/rotation.py`: [`Rotating`] restores every member's
//! pointlike data from the starting copy and applies one whole-family
//! rotation at `rate(time_spanned(α))·angle` — an absolute pose per
//! frame, never an increment, which is what makes it pure. [`rotate`]
//! (`Rotate`, rotation.py:57) is its `smooth`/`π`/1 s parameterization
//! about the box center.

use fmn_core::constants::{OUT, PI, TAU};
use fmn_core::types::Vec3;
use fmn_mobject::{Mob, Stage};

use crate::animation::{
    AnimConfig, AnimError, AnimState, Animation, AnimationSignature, RateFunc, time_spanned_alpha,
};

/// `Rotating` (rotation.py:17): Reference defaults `angle = TAU`,
/// `axis = OUT`, `run_time = 5`, `rate_func = linear`.
#[derive(Debug, Clone)]
pub struct Rotating {
    state: AnimState,
    angle: f64,
    axis: Vec3,
    about_point: Option<Vec3>,
    about_edge: Option<Vec3>,
}

impl Rotating {
    /// `Rotating(mobject)` with the Reference defaults.
    #[must_use]
    pub fn new(mobject: Mob) -> Self {
        let config = AnimConfig {
            name: "Rotating".to_owned(),
            run_time: 5.0,
            rate_func: RateFunc::linear(),
            ..AnimConfig::default()
        };
        Self {
            state: AnimState::new(mobject, config),
            angle: TAU,
            axis: OUT,
            about_point: None,
            about_edge: None,
        }
    }

    /// The full rotation angle.
    #[must_use]
    pub fn with_angle(mut self, angle: f64) -> Self {
        self.angle = angle;
        self
    }

    /// The rotation axis.
    #[must_use]
    pub fn with_axis(mut self, axis: Vec3) -> Self {
        self.axis = axis;
        self
    }

    /// Pivot on an explicit point.
    #[must_use]
    pub fn with_about_point(mut self, point: Vec3) -> Self {
        self.about_point = Some(point);
        self
    }

    /// Pivot on the box point toward `edge`.
    #[must_use]
    pub fn with_about_edge(mut self, edge: Vec3) -> Self {
        self.about_edge = Some(edge);
        self
    }

    /// Replace the animation config.
    #[must_use]
    pub fn with_config(mut self, config: AnimConfig) -> Self {
        self.state.config = config;
        self
    }
}

impl Animation for Rotating {
    fn state(&self) -> &AnimState {
        &self.state
    }

    fn state_mut(&mut self) -> &mut AnimState {
        &mut self.state
    }

    /// Pure: each frame writes the absolute pose
    /// `rotate(start, rate(α)·angle)` — a function of the begin snapshot
    /// and alpha (fmn-dmath trig, per the determinism contract).
    fn effect_signature(&self) -> AnimationSignature {
        AnimationSignature::Pure
    }

    fn setup(&mut self, stage: &mut Stage) -> Result<(), AnimError> {
        let mobject = self.state.mobject();
        if !stage.contains(mobject) {
            return Err(AnimError::StaleHandle(mobject));
        }
        Ok(())
    }

    /// rotation.py:42 `interpolate_mobject`: restore pointlike data
    /// member-for-member from the start, then one family rotation at
    /// `rate(time_spanned(α))·angle`. Lag never applies (the Reference's
    /// override bypasses the per-submobject pipeline).
    fn interpolate(&mut self, stage: &mut Stage, alpha: f64) {
        let Some(starting) = self.state.starting_mobject() else {
            return;
        };
        let mobject = self.state.mobject();
        let family = stage.family(mobject);
        let start_family = stage.family(starting);
        for (&sub, &start) in family.iter().zip(start_family.iter()) {
            stage.match_points(sub, start);
        }
        let config = &self.state.config;
        let spanned = time_spanned_alpha(alpha, config.run_time, config.time_span);
        let turn = config.rate_func.eval(spanned) * self.angle;
        stage.rotate(mobject, turn, self.axis, self.about_point, self.about_edge);
    }

    fn interpolate_submobject(&mut self, _stage: &mut Stage, _mobs: &[Mob], _sub_alpha: f64) {
        // Unreachable: interpolate is overridden and never zips families.
    }
}

/// `Rotate` (rotation.py:57): `angle = π`, `run_time = 1`, `smooth`,
/// `about_edge = ORIGIN` (the box center).
#[must_use]
pub fn rotate(mobject: Mob, angle: f64) -> Rotating {
    let mut anim = Rotating::new(mobject)
        .with_angle(angle)
        .with_about_edge([0.0, 0.0, 0.0]);
    anim.state.config.name = "Rotate".to_owned();
    anim.state.config.run_time = 1.0;
    anim.state.config.rate_func = RateFunc::smooth();
    anim
}

/// `Rotate` with its every-default surface (`angle = π`).
#[must_use]
pub fn rotate_default(mobject: Mob) -> Rotating {
    rotate(mobject, PI)
}
