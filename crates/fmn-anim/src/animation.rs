//! The Animation contract (§9.1, fm-67a): the Reference's constructor
//! surface field-for-field, the `begin → interpolate(alpha) → finish`
//! lifecycle, per-submobject lag via `get_sub_alpha`, and the typed
//! `prepare_animation` boundary.
//!
//! Mirrors `manimlib/animation/animation.py` at the pinned commit
//! (`3b1b/manim @ 6199a00d4c1b1127ebe45cb629c3f22538b10e13`):
//!
//! - [`AnimConfig`] is the §1.3 constructor surface — `(mobject,
//!   run_time=1.0, time_span, lag_ratio=0, rate_func=smooth, name,
//!   remover=False, final_alpha_value=1.0, suspend_mobject_updating=False)`
//!   — with the mobject handle carried by [`AnimState`].
//! - The [`Animation`] trait provides the lifecycle verbatim: `begin`
//!   (time-span run-time widening → `set_animating_status(true)` → the
//!   starting-mobject copy → optional updater suspension → the zipped
//!   family table → `interpolate(0)`), `interpolate` →
//!   `interpolate_mobject` → per-pair `interpolate_submobject`, and
//!   `finish` (`interpolate(final_alpha_value)` →
//!   `set_animating_status(false)` → conditional `resume_updating`).
//!   Subclass extension happens through the [`Animation::setup`] hook (the
//!   Reference's `super().begin()` idiom, inverted so the canonical
//!   sequence is written once and cannot be reordered).
//! - **The one normalized-alpha pipeline (§9.4)**: raw segment alpha →
//!   [`time_spanned_alpha`] (the `time_span` re-window) → the lag
//!   distribution → [`RateFunc`] evaluation, exactly the Reference's
//!   `interpolate_mobject`/`get_sub_alpha` composition, implemented once in
//!   [`sub_alpha`] so every mechanism family (fm-cye) and composition
//!   operator (fm-hfe) shares it. [`RateFunc`] models `squish_rate_func`
//!   and `not_quite_there` as *composable data*, so a squish-of-squish is
//!   representable, testable, and journalable.
//! - [`prepare_animation`] accepts an animation or an `.animate` recording
//!   — [`IntoAnimation`] is implemented for `Animation` types, for
//!   [`AnimBuilder`], and for [`BuiltAnimate`], and for nothing else. The
//!   Reference raises `TypeError` on a bare bound method; here a bare
//!   method is unrepresentable at the type level, which is the typed form
//!   of the same rejection (one recording mechanism serves both front
//!   doors, per fm-yra's `IntoAnimate` seam).
//! - [`MethodAnimation`] is the concrete carrier for built `.animate`
//!   chains (the Reference's `_MethodAnimation`): source → target record
//!   lerp over families that are structurally aligned *by construction*
//!   (the target is a build-time copy of the source). Heterogeneous-pair
//!   alignment (`align_data`) and `path_arc` arcs are the Transform
//!   family's, arriving with fm-cye; until then a path-arc request or a
//!   structurally diverged pair is a precise, named error — never garbage.
//!
//! Deliberate divergences (D5): the Reference's `update_rate_info` uses
//! Python `or`, so an explicit `run_time=0` or `lag_ratio=0` is silently
//! ignored — [`Animation::update_rate_info`] takes `Option`s and `Some(0.0)`
//! *sets* zero (the truthiness trap is unrepresentable). A `time_span`
//! whose `end <= start` raises `ZeroDivisionError`-or-NaN in the Reference;
//! here it is [`AnimError::InvalidTimeSpan`] at `begin`.

use fmn_core::rate;
use fmn_mobject::animate::{AnimateArgs, AnimateError, IntoAnimate};
use fmn_mobject::{AnimBuilder, BuiltAnimate, Mob, Stage, StageError};

/// The Reference's `DEFAULT_ANIMATION_RUN_TIME`.
pub const DEFAULT_ANIMATION_RUN_TIME: f64 = 1.0;
/// The Reference's `DEFAULT_ANIMATION_LAG_RATIO`.
pub const DEFAULT_ANIMATION_LAG_RATIO: f64 = 0.0;

// ---------------------------------------------------------------- RateFunc

/// A rate function as composable data: the named catalog entries
/// ([`fmn_core::rate`]) as the base case, with the Reference's two
/// combinators — `squish_rate_func` and `not_quite_there` — as explicit
/// nodes. Composition (squish of squish, scaled squish, …) stays
/// inspectable, which is what lets the replay journal and the API schema
/// name the rate curve of any animation.
///
/// Capturing closures (the Python front door's lambdas) join this type with
/// fmn-python's bridge work; every non-capturing `fn(f64) -> f64` — the
/// entire native catalog — fits [`RateFunc::Base`] today.
#[derive(Clone, Debug)]
pub enum RateFunc {
    /// A plain rate function (any `fn(f64) -> f64`, e.g. the fmn-core
    /// catalog).
    Base(fn(f64) -> f64),
    /// The Reference's `squish_rate_func(func, a, b)`: run `inner`'s whole
    /// arc inside `[a, b]`, clamping outside; degenerate `a == b` returns
    /// `a` (kept exactly).
    Squish {
        /// The wrapped rate function.
        inner: Box<RateFunc>,
        /// Window start.
        a: f64,
        /// Window end.
        b: f64,
    },
    /// The Reference's `not_quite_there(func, proportion)`: scale `inner`
    /// to top out at `proportion`.
    Scaled {
        /// The wrapped rate function.
        inner: Box<RateFunc>,
        /// The output scale factor.
        proportion: f64,
    },
}

impl Default for RateFunc {
    /// The Reference's default: `smooth`.
    fn default() -> Self {
        Self::Base(rate::smooth)
    }
}

impl RateFunc {
    /// `smooth` — the constructor-surface default.
    #[must_use]
    pub fn smooth() -> Self {
        Self::Base(rate::smooth)
    }

    /// `linear`.
    #[must_use]
    pub fn linear() -> Self {
        Self::Base(rate::linear)
    }

    /// Wrap in `squish_rate_func(self, a, b)`.
    #[must_use]
    pub fn squish(self, a: f64, b: f64) -> Self {
        Self::Squish {
            inner: Box::new(self),
            a,
            b,
        }
    }

    /// Wrap in `not_quite_there(self, proportion)`.
    #[must_use]
    pub fn not_quite_there(self, proportion: f64) -> Self {
        Self::Scaled {
            inner: Box::new(self),
            proportion,
        }
    }

    /// Evaluate at `t`, with the Reference's exact combinator semantics.
    #[must_use]
    pub fn eval(&self, t: f64) -> f64 {
        match self {
            Self::Base(f) => f(t),
            Self::Squish { inner, a, b } => {
                // The Reference's squish_rate_func body, branch for branch.
                if a == b {
                    *a
                } else if t < *a {
                    inner.eval(0.0)
                } else if t > *b {
                    inner.eval(1.0)
                } else {
                    inner.eval((t - a) / (b - a))
                }
            }
            Self::Scaled { inner, proportion } => proportion * inner.eval(t),
        }
    }
}

// --------------------------------------------- the normalized-alpha pipeline

/// The Reference's `clip` (`utils/simple_functions.py`): branch-for-branch,
/// with no panic on a hollow interval (unlike `f64::clamp`) — the pipeline
/// must stay total.
fn clip(value: f64, lower: f64, upper: f64) -> f64 {
    if value < lower {
        lower
    } else if value > upper {
        upper
    } else {
        value
    }
}

/// The `time_span` re-window — the Reference's `time_spanned_alpha`,
/// formula for formula: with a span `(start, end)` inside a group interval,
/// `clip(alpha·run_time − start, 0, end − start) / (end − start)`; without
/// one, the identity.
///
/// `run_time` is the *widened* run time (`begin` raises it to at least
/// `end`). Callers validate `end > start` ([`AnimError::InvalidTimeSpan`])
/// before entering the pipeline.
#[must_use]
pub fn time_spanned_alpha(alpha: f64, run_time: f64, time_span: Option<(f64, f64)>) -> f64 {
    match time_span {
        Some((start, end)) => clip(alpha * run_time - start, 0.0, end - start) / (end - start),
        None => alpha,
    }
}

/// The lag distribution + rate evaluation — the Reference's
/// `get_sub_alpha`, formula for formula:
///
/// ```text
/// full_length = (num_submobjects − 1) · lag_ratio + 1
/// value       = alpha · full_length
/// lower       = index · lag_ratio
/// sub_alpha   = rate_func(clip(value − lower, 0, 1))
/// ```
///
/// `lag_ratio = 0` applies to all submobjects simultaneously; `1` strictly
/// successively; between, with lagged starts. This is the ONE place the
/// rate function enters the pipeline (§9.4) — segment alphas arrive raw.
#[must_use]
pub fn sub_alpha(
    alpha: f64,
    index: usize,
    num_submobjects: usize,
    lag_ratio: f64,
    rate_func: &RateFunc,
) -> f64 {
    let full_length = (num_submobjects as f64 - 1.0) * lag_ratio + 1.0;
    let value = alpha * full_length;
    let lower = index as f64 * lag_ratio;
    rate_func.eval(clip(value - lower, 0.0, 1.0))
}

// ------------------------------------------------------------------ errors

/// Errors from the animation contract.
#[derive(Debug, Clone, PartialEq)]
pub enum AnimError {
    /// A mobject handle (the animated mobject, or a builder reference) is
    /// dead at begin/prepare time.
    StaleHandle(Mob),
    /// `time_span` with `end <= start` — the Reference divides by zero or
    /// produces NaN; we refuse it by name at `begin`.
    InvalidTimeSpan {
        /// The requested span start.
        start: f64,
        /// The requested span end.
        end: f64,
    },
    /// A `.animate` pair whose source and target have structurally
    /// diverged since build (family shape or record length). Alignment of
    /// heterogeneous pairs is `align_data` — the Transform family's
    /// mechanism, arriving with fm-cye.
    UnalignedFamilies,
    /// `path_arc` was recorded on the chain: arc paths are the Transform
    /// family's `path_func` mechanism, arriving with fm-cye. Named, never
    /// silently a straight line.
    PathArcUnsupported,
    /// The `.animate` build failed (stale handle, chaining-rule violation).
    Builder(AnimateError),
    /// The segment's run time was refused by the clock (non-finite, or
    /// beyond the frame counter's range).
    Clock(crate::clock::ClockError),
    /// Frame reconstruction was asked of a stateful (or skipped) segment —
    /// only pure segments carry a begin-state snapshot to reconstruct from
    /// (§9.5).
    SegmentNotPure,
    /// `MoveToTarget` on a mobject that never ran `generate_target` (the
    /// Reference's "MoveToTarget called on mobject without target").
    MissingTarget,
    /// `Restore` on a mobject that never ran `save_state`.
    MissingSavedState,
    /// The Stage refused an alignment or copy operation (malformed point
    /// run, mixed schemas).
    Stage(StageError),
}

impl std::fmt::Display for AnimError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::StaleHandle(_) => write!(f, "animation target handle is stale"),
            Self::InvalidTimeSpan { start, end } => {
                write!(f, "time_span ({start}, {end}) must satisfy end > start")
            }
            Self::UnalignedFamilies => write!(
                f,
                "source and target families diverged since build; \
                 heterogeneous alignment (align_data) lands with the Transform family (fm-cye)"
            ),
            Self::PathArcUnsupported => write!(
                f,
                "path_arc is the Transform family's path_func mechanism (fm-cye); \
                 not yet available on built .animate chains"
            ),
            Self::Builder(e) => write!(f, "animate build failed: {e}"),
            Self::Clock(e) => write!(f, "segment refused by the clock: {e}"),
            Self::SegmentNotPure => write!(
                f,
                "frame reconstruction requires a pure, unskipped segment's begin state"
            ),
            Self::MissingTarget => write!(
                f,
                "MoveToTarget called on mobject without a generated target"
            ),
            Self::MissingSavedState => {
                write!(f, "Restore called on mobject without a saved state")
            }
            Self::Stage(e) => write!(f, "stage refused the operation: {e}"),
        }
    }
}

impl std::error::Error for AnimError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Builder(e) => Some(e),
            Self::Clock(e) => Some(e),
            _ => None,
        }
    }
}

impl From<StageError> for AnimError {
    fn from(e: StageError) -> Self {
        Self::Stage(e)
    }
}

impl From<AnimateError> for AnimError {
    fn from(e: AnimateError) -> Self {
        Self::Builder(e)
    }
}

// ----------------------------------------------------------------- config

/// The §1.3 constructor surface, field for field (minus the mobject, which
/// [`AnimState`] carries as the arena handle).
#[derive(Clone, Debug)]
pub struct AnimConfig {
    /// Seconds the animation runs (`begin` widens this to at least
    /// `time_span.1`). Reference default `1.0`.
    pub run_time: f64,
    /// `(start, end)` — the window inside a group interval the animation
    /// actually runs in ([`time_spanned_alpha`]).
    pub time_span: Option<(f64, f64)>,
    /// `0`: all submobjects simultaneously; `1`: strictly successively;
    /// between: lagged starts. Reference default `0`.
    pub lag_ratio: f64,
    /// The rate curve. Reference default `smooth`.
    pub rate_func: RateFunc,
    /// Diagnostic name (the Reference derives `ClassName + str(mobject)`
    /// when empty).
    pub name: String,
    /// Whether `finish`ing this animation removes the mobject from the
    /// scene (`clean_up_from_scene` — the scene runtime consumes
    /// [`Animation::is_remover`] at fm-5xm).
    pub remover: bool,
    /// The alpha `finish` leaves the mobject at. Reference default `1.0`.
    pub final_alpha_value: f64,
    /// Suspend the mobject's own updaters for the play (start/target
    /// copies keep updating — the Reference's documented asymmetry).
    pub suspend_mobject_updating: bool,
}

impl Default for AnimConfig {
    fn default() -> Self {
        Self {
            run_time: DEFAULT_ANIMATION_RUN_TIME,
            time_span: None,
            lag_ratio: DEFAULT_ANIMATION_LAG_RATIO,
            rate_func: RateFunc::default(),
            name: String::new(),
            remover: false,
            final_alpha_value: 1.0,
            suspend_mobject_updating: false,
        }
    }
}

impl AnimConfig {
    /// Map a built `.animate` chain's [`AnimateArgs`] onto the constructor
    /// surface (unset fields take the Reference defaults). `path_arc` is
    /// not part of this surface — [`MethodAnimation::new`] rejects it by
    /// name until fm-cye.
    #[must_use]
    pub fn from_animate_args(args: &AnimateArgs) -> Self {
        Self {
            run_time: args.run_time.unwrap_or(DEFAULT_ANIMATION_RUN_TIME),
            time_span: args.time_span,
            lag_ratio: args.lag_ratio.unwrap_or(DEFAULT_ANIMATION_LAG_RATIO),
            rate_func: args
                .rate_func
                .map_or_else(RateFunc::default, RateFunc::Base),
            ..Self::default()
        }
    }
}

// ------------------------------------------------------------------ state

/// The per-play runtime state every animation carries: the animated
/// mobject, the config, and what `begin` establishes (the starting copy,
/// the zipped family table, the updater-suspension memo).
#[derive(Debug, Clone)]
pub struct AnimState {
    mobject: Mob,
    /// The constructor surface (public: the Reference exposes these as
    /// plain attributes).
    pub config: AnimConfig,
    starting_mobject: Option<Mob>,
    families: Vec<Vec<Mob>>,
    mobject_was_updating: bool,
}

impl AnimState {
    /// A fresh state for `mobject` under `config`.
    #[must_use]
    pub fn new(mobject: Mob, config: AnimConfig) -> Self {
        Self {
            mobject,
            config,
            starting_mobject: None,
            families: Vec::new(),
            mobject_was_updating: false,
        }
    }

    /// The animated mobject.
    #[must_use]
    pub fn mobject(&self) -> Mob {
        self.mobject
    }

    /// The begin-time copy (`None` before `begin`).
    #[must_use]
    pub fn starting_mobject(&self) -> Option<Mob> {
        self.starting_mobject
    }

    /// The zipped family table `begin` established (rows follow
    /// [`Animation::all_mobjects`] order; truncated to the shortest family,
    /// as Python `zip` does).
    #[must_use]
    pub fn families(&self) -> &[Vec<Mob>] {
        &self.families
    }
}

// -------------------------------------------------------------- signature

/// An animation's declared effect signature — §9.5's purity vocabulary
/// (shared with the §13.4 effect model). Membership in `Pure` is a closed
/// allowlist: only animations whose every interpolation provably writes
/// state as a function of the begin snapshot and alpha alone declare it.
/// The default is [`AnimationSignature::Unclassified`], and the
/// conservative rule (R20) demotes unclassified animations' segments to
/// stateful — misclassification would be a correctness bug, so unknown
/// always means impure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AnimationSignature {
    /// Provably pure interpolation (allowlist member).
    Pure,
    /// No declaration — conservatively treated as effectful.
    Unclassified,
}

// ------------------------------------------------------------------ trait

/// The Animation lifecycle contract (§9.1). The canonical sequences are
/// written once as provided methods; concrete animations implement
/// [`Animation::interpolate_submobject`] (the Reference's "typically
/// implemented by subclass") and extend `begin` through the
/// [`Animation::setup`] hook rather than overriding the order.
pub trait Animation {
    /// The runtime state (mobject handle, config, begin-time products).
    fn state(&self) -> &AnimState;

    /// Mutable runtime state.
    fn state_mut(&mut self) -> &mut AnimState;

    /// Subclass extension slot, run at the top of [`Animation::begin`]
    /// *before* the canonical sequence — where the Transform family will
    /// generate/align its target (fm-cye) and where [`MethodAnimation`]
    /// verifies pair alignment. Default: nothing.
    fn setup(&mut self, stage: &mut Stage) -> Result<(), AnimError> {
        let _ = stage;
        Ok(())
    }

    /// The Reference's `create_starting_mobject`: a deep family copy of the
    /// animated mobject (updater callables shared by reference, per §8.3).
    fn create_starting_mobject(&self, stage: &mut Stage) -> Result<Mob, AnimError> {
        let mobject = self.state().mobject();
        stage
            .copy_family(mobject)
            .map_err(|_| AnimError::StaleHandle(mobject))
    }

    /// The Reference's `get_all_mobjects`: ordering **must** match the
    /// argument order [`Animation::interpolate_submobject`] receives its
    /// family row in. Default: `(mobject, starting_mobject)`.
    fn all_mobjects(&self) -> Vec<Mob> {
        let state = self.state();
        let mut mobs = vec![state.mobject()];
        if let Some(starting) = state.starting_mobject() {
            mobs.push(starting);
        }
        mobs
    }

    /// Interpolate one zipped family row (`mobs` follows
    /// [`Animation::all_mobjects`] order) at pipeline-final `sub_alpha`.
    fn interpolate_submobject(&mut self, stage: &mut Stage, mobs: &[Mob], sub_alpha: f64);

    /// The Reference's `begin`, sequence for sequence: called right as the
    /// play starts; as much initialization as possible lives here.
    ///
    /// # Errors
    /// [`AnimError::InvalidTimeSpan`], [`AnimError::StaleHandle`], and
    /// whatever [`Animation::setup`] reports.
    fn begin(&mut self, stage: &mut Stage) -> Result<(), AnimError> {
        self.setup(stage)?;
        // 1. time_span widens run_time to cover its end (the Reference
        //    mutates run_time here, and get_run_time repeats the max).
        if let Some((start, end)) = self.state().config.time_span {
            if end <= start {
                return Err(AnimError::InvalidTimeSpan { start, end });
            }
            let state = self.state_mut();
            state.config.run_time = state.config.run_time.max(end);
        }
        // 2. Mark the family (and its ancestors) animating.
        let mobject = self.state().mobject();
        if !stage.contains(mobject) {
            return Err(AnimError::StaleHandle(mobject));
        }
        stage.set_animating_status(mobject, true, true);
        // 3. The starting copy.
        let starting = self.create_starting_mobject(stage)?;
        self.state_mut().starting_mobject = Some(starting);
        // 4. Optional updater suspension (remembering whether the mobject
        //    was updating, so finish only resumes what begin paused).
        if self.state().config.suspend_mobject_updating {
            let was_updating = !stage.is_updating_suspended(mobject);
            self.state_mut().mobject_was_updating = was_updating;
            stage.suspend_updating(mobject, true);
        }
        // 5. The zipped family table (Python zip: truncate to shortest).
        let all = self.all_mobjects();
        let families: Vec<Vec<Mob>> = all.iter().map(|&m| stage.family(m)).collect();
        let rows = families.iter().map(Vec::len).min().unwrap_or(0);
        self.state_mut().families = (0..rows)
            .map(|i| families.iter().map(|f| f[i]).collect())
            .collect();
        // 6. The zero interpolation — separate from emission (§9.2: no
        //    alpha-zero frame is ever emitted; this is where alpha zero
        //    happens instead).
        self.interpolate(stage, 0.0);
        // 7. Post-begin subclass hook — the Reference's Transform locks
        //    matching data *after* `super().begin()` (transform.py:54);
        //    this slot keeps that order with the canonical sequence
        //    written once.
        self.after_begin(stage);
        Ok(())
    }

    /// Subclass slot run at the end of [`Animation::begin`], after the
    /// zero interpolation (the Reference's post-`super().begin()` code —
    /// Transform's `lock_matching_data` call site). Default: nothing.
    fn after_begin(&mut self, stage: &mut Stage) {
        let _ = stage;
    }

    /// The Reference's `finish`: land on `final_alpha_value`, clear the
    /// animating status, resume updating if `begin` paused it. Scene
    /// removal for removers is the runtime's (`clean_up_from_scene`,
    /// fm-5xm) via [`Animation::is_remover`].
    fn finish(&mut self, stage: &mut Stage) {
        let final_alpha = self.state().config.final_alpha_value;
        self.interpolate(stage, final_alpha);
        let mobject = self.state().mobject();
        stage.set_animating_status(mobject, false, true);
        if self.state().config.suspend_mobject_updating && self.state().mobject_was_updating {
            // Reference resume_updating defaults: recurse and one
            // call_updater pass (which runs update(0) exactly once — C-5).
            stage.resume_updating(mobject, true, true);
        }
        // Subclass slot — the Reference's Transform unlocks data in its
        // `finish` override (transform.py:74).
        self.teardown(stage);
    }

    /// Subclass slot run at the end of [`Animation::finish`] (the
    /// Reference's `finish` overrides — Transform's `unlock_data` call
    /// site). Default: nothing.
    fn teardown(&mut self, stage: &mut Stage) {
        let _ = stage;
    }

    /// The mean of an animation: the Reference's `interpolate` (alias of
    /// `interpolate_mobject`). `alpha` is the raw segment alpha — the
    /// pipeline (`time_span` → lag → rate) applies inside, once.
    fn interpolate(&mut self, stage: &mut Stage, alpha: f64) {
        let state = self.state();
        let families = state.families.clone();
        let run_time = state.config.run_time;
        let time_span = state.config.time_span;
        let lag_ratio = state.config.lag_ratio;
        let rate_func = state.config.rate_func.clone();
        let num = families.len();
        let spanned = time_spanned_alpha(alpha, run_time, time_span);
        for (index, mobs) in families.iter().enumerate() {
            let sa = sub_alpha(spanned, index, num, lag_ratio, &rate_func);
            self.interpolate_submobject(stage, mobs, sa);
        }
    }

    /// The Reference's `update_mobjects(dt)`: tick the updaters of every
    /// animation-owned mobject (starting/target copies) — the scene handles
    /// `self.mobject` itself. Step one of the six-step frame order
    /// (fm-x79).
    fn update_mobjects(&mut self, stage: &mut Stage, dt: f64) {
        for mob in self.mobjects_to_update() {
            stage.update_mobject(mob, dt);
        }
    }

    /// The Reference's `get_all_mobjects_to_update`: everything in
    /// [`Animation::all_mobjects`] except the animated mobject itself,
    /// deduplicated, insertion order kept.
    fn mobjects_to_update(&self) -> Vec<Mob> {
        let mobject = self.state().mobject();
        let mut out: Vec<Mob> = Vec::new();
        for mob in self.all_mobjects() {
            if mob != mobject && !out.contains(&mob) {
                out.push(mob);
            }
        }
        out
    }

    /// The Reference's `get_run_time`: widened by `time_span` without
    /// mutating (begin performs the same max in place).
    fn get_run_time(&self) -> f64 {
        let config = &self.state().config;
        match config.time_span {
            Some((_, end)) => config.run_time.max(end),
            None => config.run_time,
        }
    }

    /// The Reference's `update_rate_info`: batch-update timing fields,
    /// `None` keeping the current value.
    ///
    /// Deliberate divergence (D5, module doc): the Reference's `or`-based
    /// merge silently ignores explicit zeros; here `Some(0.0)` sets zero.
    fn update_rate_info(
        &mut self,
        run_time: Option<f64>,
        rate_func: Option<RateFunc>,
        lag_ratio: Option<f64>,
    ) {
        let config = &mut self.state_mut().config;
        if let Some(rt) = run_time {
            config.run_time = rt;
        }
        if let Some(rf) = rate_func {
            config.rate_func = rf;
        }
        if let Some(lr) = lag_ratio {
            config.lag_ratio = lr;
        }
    }

    /// Whether finishing removes the mobject from the scene.
    fn is_remover(&self) -> bool {
        self.state().config.remover
    }

    /// The declared effect signature (§9.5). Default: unclassified —
    /// which the segment-purity classifier conservatively demotes (R20).
    /// Override to [`AnimationSignature::Pure`] only for animations whose
    /// interpolation is provably a function of (begin snapshot, alpha).
    fn effect_signature(&self) -> AnimationSignature {
        AnimationSignature::Unclassified
    }
}

// ------------------------------------------------------- MethodAnimation

/// The concrete animation a built `.animate` chain becomes (the
/// Reference's `_MethodAnimation`, which is Transform-family): source →
/// target field lerp over families aligned by construction. `align_data`
/// for heterogeneous pairs and `path_arc` arcs arrive with fm-cye's
/// Transform; this carrier refuses both by name.
#[derive(Debug, Clone)]
pub struct MethodAnimation {
    state: AnimState,
    target: Mob,
}

impl MethodAnimation {
    /// Wrap a built recording. Fails on a recorded `path_arc`
    /// ([`AnimError::PathArcUnsupported`]) — never a silent straight line.
    pub fn new(built: BuiltAnimate) -> Result<Self, AnimError> {
        if built.args.path_arc.is_some_and(|arc| arc != 0.0) {
            return Err(AnimError::PathArcUnsupported);
        }
        let mut config = AnimConfig::from_animate_args(&built.args);
        if config.name.is_empty() {
            config.name = "MethodAnimation".to_owned();
        }
        Ok(Self {
            state: AnimState::new(built.source, config),
            target: built.target,
        })
    }

    /// The build-time target copy the play interpolates toward.
    #[must_use]
    pub fn target(&self) -> Mob {
        self.target
    }
}

impl Animation for MethodAnimation {
    fn state(&self) -> &AnimState {
        &self.state
    }

    fn state_mut(&mut self) -> &mut AnimState {
        &mut self.state
    }

    /// Allowlist member: the interpolation below writes records as a pure
    /// function of the frozen start/target pair and alpha.
    fn effect_signature(&self) -> AnimationSignature {
        AnimationSignature::Pure
    }

    /// Verify the by-construction alignment still holds (family shape and
    /// per-pair record length); a diverged pair is the fm-cye boundary,
    /// reported by name.
    fn setup(&mut self, stage: &mut Stage) -> Result<(), AnimError> {
        let source = self.state.mobject();
        if !stage.contains(source) {
            return Err(AnimError::StaleHandle(source));
        }
        if !stage.contains(self.target) {
            return Err(AnimError::StaleHandle(self.target));
        }
        let source_family = stage.family(source);
        let target_family = stage.family(self.target);
        if source_family.len() != target_family.len() {
            return Err(AnimError::UnalignedFamilies);
        }
        for (&s, &t) in source_family.iter().zip(&target_family) {
            let (Some(se), Some(te)) = (stage.get(s), stage.get(t)) else {
                return Err(AnimError::UnalignedFamilies);
            };
            if se.buffer.len() != te.buffer.len() {
                return Err(AnimError::UnalignedFamilies);
            }
        }
        Ok(())
    }

    /// Transform-family ordering: `(mobject, starting_mobject, target)` —
    /// the argument order the family rows arrive in.
    fn all_mobjects(&self) -> Vec<Mob> {
        let mut mobs = vec![self.state.mobject()];
        if let Some(starting) = self.state.starting_mobject() {
            mobs.push(starting);
        }
        mobs.push(self.target);
        mobs
    }

    /// Straight field lerp `start → target` written into the live
    /// submobject, every record field, computed in f64 and stored back at
    /// record precision (§6.1's mixed-precision doctrine). Uniforms are
    /// untouched: no [`fmn_mobject::animate::AnimateCommand`] records a
    /// uniform write, so both endpoints agree by construction (full
    /// uniform interpolation is Transform's, fm-cye).
    fn interpolate_submobject(&mut self, stage: &mut Stage, mobs: &[Mob], sub_alpha: f64) {
        let [submob, starting, target] = *mobs else {
            return; // rows are triples by all_mobjects; anything else is pre-begin
        };
        let fields: Vec<String> = match stage.get(submob) {
            Some(entry) => entry
                .buffer
                .schema()
                .fields()
                .iter()
                .map(|f| f.name.clone())
                .collect(),
            None => return,
        };
        for field in fields {
            let (Some(from), Some(to)) = (
                stage
                    .get(starting)
                    .and_then(|e| e.buffer.read_column(&field)),
                stage.get(target).and_then(|e| e.buffer.read_column(&field)),
            ) else {
                continue;
            };
            debug_assert_eq!(
                from.len(),
                to.len(),
                "setup verified pair alignment; structural mutation mid-play \
                 is unreachable through the frame model"
            );
            if from.len() != to.len() {
                continue;
            }
            let lerped: Vec<f32> = from
                .iter()
                .zip(&to)
                .map(|(&a, &b)| {
                    let a = f64::from(a);
                    let b = f64::from(b);
                    ((1.0 - sub_alpha) * a + sub_alpha * b) as f32
                })
                .collect();
            if let Some(entry) = stage.get_mut(submob) {
                entry.buffer.write_range(&field, 0, &lerped);
            }
        }
    }
}

// ----------------------------------------------------- prepare_animation

/// The typed `prepare_animation` contract (§9.1): an animation, or an
/// `.animate` recording — nothing else. The Reference's runtime
/// `TypeError` on bare bound methods is a compile error here: no other
/// type implements this trait.
pub trait IntoAnimation {
    /// Realize into a boxed [`Animation`].
    ///
    /// # Errors
    /// [`AnimError`] from the `.animate` build or carrier construction.
    fn into_animation(self, stage: &mut Stage) -> Result<Box<dyn Animation>, AnimError>;
}

impl<A: Animation + 'static> IntoAnimation for A {
    fn into_animation(self, stage: &mut Stage) -> Result<Box<dyn Animation>, AnimError> {
        let _ = stage;
        Ok(Box::new(self))
    }
}

impl IntoAnimation for AnimBuilder {
    fn into_animation(self, stage: &mut Stage) -> Result<Box<dyn Animation>, AnimError> {
        let built = self.prepare(stage)?;
        Ok(Box::new(MethodAnimation::new(built)?))
    }
}

impl IntoAnimation for BuiltAnimate {
    fn into_animation(self, stage: &mut Stage) -> Result<Box<dyn Animation>, AnimError> {
        let built = self.prepare(stage)?; // revalidates handles, as IntoAnimate does
        Ok(Box::new(MethodAnimation::new(built)?))
    }
}

/// The Reference's `prepare_animation` entry point, shared by both front
/// doors: builders build (dynamic target lookup happens NOW, at play
/// time), animations pass through.
///
/// # Errors
/// [`AnimError`] from the build or carrier construction.
pub fn prepare_animation(
    input: impl IntoAnimation,
    stage: &mut Stage,
) -> Result<Box<dyn Animation>, AnimError> {
    input.into_animation(stage)
}
