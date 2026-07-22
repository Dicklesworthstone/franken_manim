//! The indication mechanism family — family 4 (§9.4, fm-cye), ported
//! from the pinned Reference's `animation/indication.py`. The classes
//! that are pure mechanism land here:
//!
//! - [`indicate`] (indication.py:73): a Transform whose target is a
//!   scaled, recolored copy under `there_and_back` — the mobject swells,
//!   flushes, and returns.
//! - [`WiggleOutThenIn`] (indication.py:357): per frame, restore the
//!   pointlike data and apply an absolute scale-and-rotate about fixed
//!   pivots (`there_and_back` for the swell, the `wiggle` rate for the
//!   rocking) — closure-free and provably pure.
//! - [`turn_inside_out`] (indication.py:401): transform onto a
//!   points-reversed copy along a 90° arc. The Reference calls the
//!   nonexistent `refresh_triangulation` (defect C-1, Appendix C: *fixed —
//!   the evident intent implemented*): FrankenManim's renderer has no
//!   triangulation to refresh, and the record-revision bump the reversal
//!   causes is the intent — caches can never serve the stale orientation.
//! - [`VShowPassingFlash`] (indication.py:184): a gaussian stroke-width
//!   window (σ = time_width/6, ±3σ support) over per-member width
//!   profiles tapered at the ends, swept across the path; `finish`
//!   restores every member's style. [`show_creation_then_destruction`]
//!   (indication.py:284) parameterizes the plain passing flash at
//!   `time_width = 2`.
//! - [`apply_wave`] (indication.py:333): the Homotopy whose nudge is
//!   `there_and_back(t^power)·amplitude·direction`, power sliding with
//!   the x-proportion — functional-map family under an indication name.
//!
//! The rest of the module is geometry- or composition-bound and lands
//! with its dependencies (the seams the fm-cye bead records): `FocusOn`,
//! `CircleIndicate`, `FlashAround`/`FlashUnder` need `Dot`/`Circle`/
//! `SurroundingRectangle`/`Underline` (fmn-library, §12); `Flash`,
//! `ShowCreationThenFadeOut`, the `AnimationOnSurroundingRectangle`
//! family, `FlashyFadeIn`, and specialized.py's `Broadcast` need
//! `AnimationGroup`/`Succession`/`LaggedStart` (fm-hfe). Their mechanisms
//! — Transform, passing flash, fade, restore — are all already here.

use fmn_core::rate;
use fmn_core::types::Vec3;
use fmn_mobject::{Mob, Stage};

use crate::animation::{AnimConfig, AnimError, AnimState, Animation, AnimationSignature, RateFunc};
use crate::creation::match_style_from;
use crate::movement::Homotopy;
use crate::transform::{Transform, set_family_rgb};

/// The Reference's `YELLOW` (`#FFFF00`), the indication default color.
pub const INDICATION_YELLOW: [f32; 3] = [1.0, 1.0, 0.0];

/// `Indicate` (indication.py:73): transform onto a copy scaled by
/// `scale_factor` and recolored, under `there_and_back`. Reference
/// defaults: `scale_factor = 1.2`, `color = YELLOW`.
///
/// # Errors
/// [`AnimError::StaleHandle`] / [`AnimError::Stage`].
pub fn indicate(
    stage: &mut Stage,
    mobject: Mob,
    scale_factor: f64,
    color: Option<[f32; 3]>,
) -> Result<Transform, AnimError> {
    let target = stage.copy_family(mobject)?;
    stage.scale(target, scale_factor);
    set_family_rgb(stage, target, color.unwrap_or(INDICATION_YELLOW));
    let mut t = Transform::new(mobject, target);
    t.state_mut().config.name = "Indicate".to_owned();
    t.state_mut().config.rate_func = RateFunc::Base(rate::there_and_back);
    Ok(t)
}

/// `TurnInsideOut` (indication.py:401): transform onto a points-reversed
/// copy along a `path_arc` (default 90°). C-1's ruling applies — see the
/// module docs.
///
/// # Errors
/// [`AnimError::StaleHandle`] / [`AnimError::Stage`].
pub fn turn_inside_out(
    stage: &mut Stage,
    mobject: Mob,
    path_arc: f64,
) -> Result<Transform, AnimError> {
    let target = stage.copy_family(mobject)?;
    stage.reverse_family_points(target)?;
    let mut t = Transform::new(mobject, target).with_path_arc(path_arc, [0.0, 0.0, 1.0]);
    t.state_mut().config.name = "TurnInsideOut".to_owned();
    Ok(t)
}

// ------------------------------------------------------ WiggleOutThenIn

/// `WiggleOutThenIn` (indication.py:357): swell by `there_and_back`,
/// rock by the `wiggle` rate — absolute pose per frame. Reference
/// defaults: `scale_value = 1.1`, `rotation_angle = 0.01·TAU`,
/// `n_wiggles = 6`, `run_time = 2`.
#[derive(Debug, Clone)]
pub struct WiggleOutThenIn {
    state: AnimState,
    scale_value: f64,
    rotation_angle: f64,
    n_wiggles: f64,
    scale_about_point: Option<Vec3>,
    rotate_about_point: Option<Vec3>,
}

impl WiggleOutThenIn {
    /// Reference defaults.
    #[must_use]
    pub fn new(mobject: Mob) -> Self {
        let config = AnimConfig {
            name: "WiggleOutThenIn".to_owned(),
            run_time: 2.0,
            ..AnimConfig::default()
        };
        Self {
            state: AnimState::new(mobject, config),
            scale_value: 1.1,
            rotation_angle: 0.01 * fmn_core::constants::TAU,
            n_wiggles: 6.0,
            scale_about_point: None,
            rotate_about_point: None,
        }
    }

    /// The swell factor.
    #[must_use]
    pub fn with_scale_value(mut self, value: f64) -> Self {
        self.scale_value = value;
        self
    }

    /// The rocking amplitude in radians.
    #[must_use]
    pub fn with_rotation_angle(mut self, angle: f64) -> Self {
        self.rotation_angle = angle;
        self
    }

    /// The wiggle count.
    #[must_use]
    pub fn with_n_wiggles(mut self, n: f64) -> Self {
        self.n_wiggles = n;
        self
    }

    /// Replace the animation config.
    #[must_use]
    pub fn with_config(mut self, config: AnimConfig) -> Self {
        self.state.config = config;
        self
    }
}

impl Animation for WiggleOutThenIn {
    fn state(&self) -> &AnimState {
        &self.state
    }

    fn state_mut(&mut self) -> &mut AnimState {
        &mut self.state
    }

    /// Pure: restore-then-transform about pivots that are functions of
    /// the begin state (the default pivots read the restored center).
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

    /// indication.py:384: `match_points(start)`, scale by
    /// `interpolate(1, scale_value, there_and_back(α))`, rotate by
    /// `wiggle(α, n)·rotation_angle`, each about its pivot.
    fn interpolate_submobject(&mut self, stage: &mut Stage, mobs: &[Mob], sub_alpha: f64) {
        let [submob, start] = *mobs else {
            return; // rows are pairs once begin has run
        };
        stage.match_points(submob, start);
        let mobject = self.state.mobject();
        let scale_pivot = self
            .scale_about_point
            .unwrap_or_else(|| stage.get_center(mobject));
        let rotate_pivot = self
            .rotate_about_point
            .unwrap_or_else(|| stage.get_center(mobject));
        let swell = rate::there_and_back(sub_alpha);
        let factor = (1.0 - swell) + swell * self.scale_value;
        stage.scale_about(submob, factor, Some(scale_pivot), None);
        stage.rotate(
            submob,
            rate::wiggle(sub_alpha, self.n_wiggles) * self.rotation_angle,
            [0.0, 0.0, 1.0],
            Some(rotate_pivot),
            None,
        );
    }
}

// ----------------------------------------------------- VShowPassingFlash

/// `VShowPassingFlash` (indication.py:184): sweep a gaussian
/// stroke-width window over tapered per-member width profiles. Reference
/// defaults: `time_width = 0.3`, `taper_width = 0.05`, remover.
#[derive(Debug, Clone)]
pub struct VShowPassingFlash {
    state: AnimState,
    time_width: f64,
    taper_width: f64,
    widths: Vec<(Mob, Vec<f64>)>,
}

impl VShowPassingFlash {
    /// Reference defaults.
    #[must_use]
    pub fn new(vmobject: Mob) -> Self {
        let config = AnimConfig {
            name: "VShowPassingFlash".to_owned(),
            remover: true,
            ..AnimConfig::default()
        };
        Self {
            state: AnimState::new(vmobject, config),
            time_width: 0.3,
            taper_width: 0.05,
            widths: Vec::new(),
        }
    }

    /// The gaussian window's width in proportion units.
    #[must_use]
    pub fn with_time_width(mut self, time_width: f64) -> Self {
        self.time_width = time_width;
        self
    }

    /// The end-taper width in proportion units.
    #[must_use]
    pub fn with_taper_width(mut self, taper_width: f64) -> Self {
        self.taper_width = taper_width;
        self
    }

    /// Replace the animation config.
    #[must_use]
    pub fn with_config(mut self, config: AnimConfig) -> Self {
        self.state.config = config;
        self
    }

    /// indication.py:198 `taper_kernel`.
    fn taper_kernel(&self, x: f64) -> f64 {
        if x < self.taper_width {
            x
        } else if x > 1.0 - self.taper_width {
            1.0 - x
        } else {
            1.0
        }
    }
}

impl Animation for VShowPassingFlash {
    fn state(&self) -> &AnimState {
        &self.state
    }

    fn state_mut(&mut self) -> &mut AnimState {
        &mut self.state
    }

    /// Pure: the width profiles freeze at begin; each frame's widths are
    /// `profile·gaussian(α)`.
    fn effect_signature(&self) -> AnimationSignature {
        AnimationSignature::Pure
    }

    /// indication.py:205 `begin`: snapshot each member's tapered width
    /// profile before the canonical sequence runs.
    fn setup(&mut self, stage: &mut Stage) -> Result<(), AnimError> {
        let mobject = self.state.mobject();
        if !stage.contains(mobject) {
            return Err(AnimError::StaleHandle(mobject));
        }
        self.widths.clear();
        for member in stage.family(mobject) {
            let Some(entry) = stage.get(member) else {
                continue;
            };
            let Some(widths) = entry.buffer.read_column("stroke_width") else {
                continue;
            };
            let n = widths.len();
            let profile: Vec<f64> = widths
                .iter()
                .enumerate()
                .map(|(i, &w)| {
                    let x = if n > 1 {
                        i as f64 / (n as f64 - 1.0)
                    } else {
                        0.0
                    };
                    f64::from(w) * self.taper_kernel(x)
                })
                .collect();
            self.widths.push((member, profile));
        }
        Ok(())
    }

    /// indication.py:222: a gaussian with 3σ = time_width/2 swept from
    /// `−tw/2` to `1 + tw/2`, zeroed outside its support.
    fn interpolate_submobject(&mut self, stage: &mut Stage, mobs: &[Mob], sub_alpha: f64) {
        let [submob, _start] = *mobs else {
            return; // rows are pairs once begin has run
        };
        let Some((_, profile)) = self.widths.iter().find(|(m, _)| *m == submob) else {
            return;
        };
        let tw = self.time_width;
        let sigma = tw / 6.0;
        let mu = (1.0 - sub_alpha) * (-tw / 2.0) + sub_alpha * (1.0 + tw / 2.0);
        let n = profile.len();
        #[allow(clippy::cast_possible_truncation)]
        let out: Vec<f32> = profile
            .iter()
            .enumerate()
            .map(|(i, &w)| {
                let x = if n > 1 {
                    i as f64 / (n as f64 - 1.0)
                } else {
                    0.0
                };
                if (x - mu).abs() > 3.0 * sigma {
                    return 0.0;
                }
                let z = (x - mu) / sigma;
                (w * fmn_dmath::exp(-0.5 * z * z)) as f32
            })
            .collect();
        if !out.is_empty()
            && let Some(entry) = stage.get_mut(submob)
        {
            entry.buffer.write_range("stroke_width", 0, &out);
        }
    }

    /// indication.py:249 `finish`: every member's style returns to the
    /// start's.
    fn teardown(&mut self, stage: &mut Stage) {
        if let Some(starting) = self.state.starting_mobject() {
            match_style_from(stage, self.state.mobject(), starting);
        }
    }
}

/// `ShowCreationThenDestruction` (indication.py:284): the plain passing
/// flash at `time_width = 2`.
#[must_use]
pub fn show_creation_then_destruction(vmobject: Mob) -> crate::creation::ShowPartial {
    let mut anim = crate::creation::show_passing_flash(vmobject, 2.0);
    anim.state_mut().config.name = "ShowCreationThenDestruction".to_owned();
    anim
}

// ------------------------------------------------------------ ApplyWave

/// `ApplyWave` (indication.py:333): a Homotopy nudging points along
/// `direction`, the nudge phased by x-proportion. Reference defaults:
/// `direction = UP`, `amplitude = 0.2`, `run_time = 1`.
#[must_use]
pub fn apply_wave(stage: &Stage, mobject: Mob, direction: Vec3, amplitude: f64) -> Homotopy {
    let left_x = stage.get_left(mobject)[0];
    let right_x = stage.get_right(mobject)[0];
    let vect = [
        amplitude * direction[0],
        amplitude * direction[1],
        amplitude * direction[2],
    ];
    let mut anim = Homotopy::new(
        move |x, y, z, t| {
            let alpha = (x - left_x) / (right_x - left_x);
            let power = fmn_dmath::exp(2.0 * (alpha - 0.5));
            let nudge = rate::there_and_back(fmn_dmath::pow(t, power));
            [
                x + nudge * vect[0],
                y + nudge * vect[1],
                z + nudge * vect[2],
            ]
        },
        mobject,
    );
    anim.state_mut().config.name = "ApplyWave".to_owned();
    anim.state_mut().config.run_time = 1.0;
    anim
}
