//! Complete native method animations over the shared Transform mechanism.
use super::{
    AnimConfig, AnimError, AnimState, Animation, AnimationSignature, IntoAnimation, IntoAnimations,
    RateFunc,
};
use crate::transform::Transform;
use fmn_mobject::animate::OverrideAnimation;
use fmn_mobject::{AnimBuilder, AnimateArgs, AnimateError, BuiltAnimate, Mob, Stage};

/// A native `.animate` method override, resolved when the scene prepares play.
///
/// The factory runs exactly once on the scene-owning thread, after validating
/// the source and explicit timing arguments. It sees the current arena, so
/// target lookups and method-specific geometry are not frozen at recording
/// time. Its returned animation retains its normal lifecycle and purity rules;
/// the factory itself never enters a frame worker or a frozen interpolation.
///
/// Explicit builder timing, rate, lag and time-span arguments override the
/// returned animation's corresponding settings. Unset arguments preserve the
/// factory's defaults, including its remover, final alpha, updater suspension
/// and diagnostic name. `path_arc` and `path_arc_axis` are passed to the factory
/// because their meaning depends on the chosen animation mechanism.
///
/// Chained mutation commands and a second override are refused at construction.
/// This type intentionally exposes no mutation-recording methods.
///
/// ```
/// use fmn_anim::{MethodOverride, prepare_animation, v_fade_out};
/// use fmn_mobject::{AnimateArgs, Mobject, Stage};
/// # fn main() -> Result<(), Box<dyn std::error::Error>> {
/// let mut stage = Stage::new();
/// let mob = stage.add(Mobject::vector_from_points(&[[0.0; 3]]));
/// let recording = mob.animate().set_anim_args(AnimateArgs {
///     run_time_frames: Some(12),
///     ..AnimateArgs::default()
/// })?;
/// let overridden = MethodOverride::new(recording, "fade-away", |_, source, _| {
///     Ok(v_fade_out(source))
/// })?;
/// let animation = prepare_animation(overridden, &mut stage)?;
/// assert!(animation.is_remover());
/// assert_eq!(animation.state().config.run_time_frames, Some(12));
/// # Ok(())
/// # }
/// ```
pub struct MethodOverride<F> {
    builder: AnimBuilder,
    name: &'static str,
    factory: F,
}

impl<F> std::fmt::Debug for MethodOverride<F> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MethodOverride")
            .field("builder", &self.builder)
            .field("name", &self.name)
            .finish_non_exhaustive()
    }
}

impl<F> MethodOverride<F> {
    /// Bind the overriding animation factory without running it or copying a
    /// target. Captured method arguments stay owned by the factory until play.
    ///
    /// # Errors
    /// [`AnimateError::OverrideNotChainable`] if the builder already recorded
    /// a mutation or an override. Factory failures propagate from preparation;
    /// as with ordinary authored constructors, factories own their mutations.
    pub fn new<A>(
        builder: AnimBuilder,
        name: &'static str,
        factory: F,
    ) -> Result<Self, AnimateError>
    where
        F: FnOnce(&mut Stage, Mob, AnimateArgs) -> Result<A, AnimError>,
        A: Animation + 'static,
    {
        let builder = builder.with_override(OverrideAnimation { name })?;
        Ok(Self {
            builder,
            name,
            factory,
        })
    }

    /// The source whose live handle will be checked before the factory runs.
    #[must_use]
    pub fn source(&self) -> Mob {
        self.builder.source()
    }

    /// Set animation arguments once, before or after binding the factory.
    ///
    /// # Errors
    /// [`AnimateError::ArgsAlreadySet`] if the original builder or this wrapper
    /// already received arguments.
    pub fn set_anim_args(mut self, args: AnimateArgs) -> Result<Self, AnimateError> {
        self.builder = self.builder.set_anim_args(args)?;
        Ok(self)
    }
}

impl<F, A> IntoAnimation for MethodOverride<F>
where
    F: FnOnce(&mut Stage, Mob, AnimateArgs) -> Result<A, AnimError>,
    A: Animation + 'static,
{
    fn into_animation(self, stage: &mut Stage) -> Result<Box<dyn Animation>, AnimError> {
        let source = self.builder.source();
        if !stage.contains(source) {
            return Err(AnimError::StaleHandle(source));
        }
        let args = self.builder.animation_args();
        validate_override_args(&args)?;
        let mut animation = (self.factory)(stage, source, args)?;
        // Use the animation's hook, not just its config: compositions retain
        // whether run_time was explicitly overridden when deriving intervals.
        animation.update_rate_info(
            args.run_time,
            args.rate_func.map(RateFunc::Base),
            args.lag_ratio,
        );
        let config = &mut animation.state_mut().config;
        if let Some(frames) = args.run_time_frames {
            config.run_time_frames = Some(frames);
        }
        if let Some(time_span) = args.time_span {
            config.time_span = Some(time_span);
        }
        if config.name.is_empty() {
            config.name = self.name.to_owned();
        }
        Ok(Box::new(animation))
    }
}

impl<F, A> IntoAnimations for MethodOverride<F>
where
    F: FnOnce(&mut Stage, Mob, AnimateArgs) -> Result<A, AnimError>,
    A: Animation + 'static,
{
    fn into_animations(self, stage: &mut Stage) -> Result<Vec<Box<dyn Animation>>, AnimError> {
        self.into_animation(stage).map(|animation| vec![animation])
    }
}

fn validate_override_args(args: &AnimateArgs) -> Result<(), AnimError> {
    if let Some((start, end)) = args.time_span
        && (!start.is_finite() || !end.is_finite() || end <= start)
    {
        return Err(AnimError::InvalidTimeSpan { start, end });
    }
    if args.lag_ratio.is_some_and(|lag| !lag.is_finite()) {
        return Err(AnimError::InvalidOverrideArgument(
            "lag_ratio must be finite",
        ));
    }
    if args.run_time_frames.is_none() && args.run_time.is_some_and(|seconds| !seconds.is_finite()) {
        // A finite time-span endpoint must not hide NaN through f64::max.
        return Err(AnimError::Clock(crate::clock::ClockError::NonFiniteRunTime));
    }
    // These checks do not assume the eventual scene FPS. Its exact frame
    // count and remaining clock budget are still validated by the play driver.
    AnimConfig::from_animate_args(args)
        .duration_on_grid(1)?
        .n_frames()
        .map_err(AnimError::Clock)?;
    Ok(())
}

/// A built `.animate` recording using the complete Transform mechanism.
///
/// The recording still resolves its target at build time. At begin, Transform
/// aligns a private target when necessary and freezes the starting family.
/// Sharing these hooks also gives method animations point-path interpolation,
/// numeric uniforms/trackers, matching-field locks, and correct host-view
/// materialization without a second, reduced implementation of Transform.
#[derive(Debug, Clone)]
pub struct MethodAnimation {
    transform: Transform,
    target: Mob,
}

impl MethodAnimation {
    /// Wrap a built recording, retaining its path and lifecycle arguments.
    /// Handle and schema validation occurs at prepare/begin, as for Transform.
    ///
    /// # Errors
    /// A named override is not a target transform and must be prepared through
    /// [`MethodOverride`] instead of silently losing its requested behavior.
    pub fn new(built: BuiltAnimate) -> Result<Self, AnimError> {
        if let Some(animation) = built.overridden {
            return Err(AnimateError::UnresolvedOverride {
                name: animation.name,
            }
            .into());
        }
        let mut config = AnimConfig::from_animate_args(&built.args);
        if config.name.is_empty() {
            config.name = "MethodAnimation".to_owned();
        }
        let transform = Transform::new(built.source, built.target)
            .with_config(config)
            .with_path_arc(
                built.args.path_arc.unwrap_or(0.0),
                built.args.path_arc_axis.unwrap_or(fmn_core::constants::OUT),
            );
        Ok(Self {
            transform,
            target: built.target,
        })
    }

    /// The original build-time target; alignment never mutates this family.
    #[must_use]
    pub fn target(&self) -> Mob {
        self.target
    }
}

impl Animation for MethodAnimation {
    fn state(&self) -> &AnimState {
        self.transform.state()
    }

    fn state_mut(&mut self) -> &mut AnimState {
        self.transform.state_mut()
    }

    fn effect_signature(&self) -> AnimationSignature {
        self.transform.effect_signature()
    }

    fn freeze_interpolation(&self) -> Option<crate::pure_frame::FrozenAnimation> {
        self.transform.freeze_interpolation()
    }

    fn setup(&mut self, stage: &mut Stage) -> Result<(), AnimError> {
        self.transform.setup(stage)
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
        self.transform.preflight_mobjects()
    }

    fn interpolate_submobject(&mut self, stage: &mut Stage, mobs: &[Mob], sub_alpha: f64) {
        self.transform
            .interpolate_submobject(stage, mobs, sub_alpha);
    }
}
