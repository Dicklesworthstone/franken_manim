//! The `.animate` builder: deferred-command recording with the Reference's
//! REAL rules (§8.6, fm-yra; G0-1's ratified fluent-recording shape — one
//! implementation serving both front doors).
//!
//! `mob.animate()` returns a recorder; chained positional calls append
//! commands; [`AnimBuilder::build`] realizes the recording against a target
//! copy generated **at build time** — the deferred-command model D-11
//! ratified (the borrow-checker-clean shape: the recorder owns only a
//! `Copy` handle). [`PosTarget::Mob`](crate::positional::PosTarget)
//! references inside commands also resolve at build time, so a `move_to`
//! recorded against a moving mobject uses its position when the play
//! happens, not when the chain was written.
//!
//! The Reference's builder rules, kept exactly (`_AnimationBuilder`):
//!
//! - **Animation arguments are set once per chain.** `animate(**kwargs)` /
//!   `set_anim_args` may be called only once; a second call raises. Here:
//!   [`AnimBuilder::set_anim_args`] errors with
//!   [`AnimateError::ArgsAlreadySet`].
//! - **Overridden animations don't chain.** A method carrying
//!   `_override_animate` may be neither preceded nor followed by another
//!   chained call. Here: [`AnimBuilder::with_override`] errors with
//!   [`AnimateError::OverrideNotChainable`] if anything was chained before
//!   it, and every recording method errors the same way after it. (The
//!   library's overridden methods arrive with W7; the rule and its
//!   enforcement live here.)
//! - **`prepare_animation` accepts an `Animation` or a builder, nothing
//!   else.** The Reference rejects bare bound methods with a `TypeError`;
//!   in Rust that contract is the [`IntoAnimate`] trait — implemented for
//!   [`AnimBuilder`] and [`BuiltAnimate`] only, so a "bare method" is
//!   unrepresentable at the type level. fmn-anim's `Animation` joins the
//!   same contract at fm-67a (§9.1 shares it).

use crate::positional::PosTarget;
use crate::stage::{Mob, Stage};
use fmn_core::types::Vec3;

/// Transform-level arguments passed once per chain (the Reference's
/// `set_anim_args` surface: `run_time`, `rate_func`, `lag_ratio`,
/// `path_arc`, `time_span`). Consumed by Choreo when the built animation is
/// played (fm-67a).
#[derive(Clone, Copy, Debug, Default)]
pub struct AnimateArgs {
    /// Seconds the transform runs; engine default when `None`.
    pub run_time: Option<f64>,
    /// The rate function (an fmn-core `rate` fn); engine default when `None`.
    pub rate_func: Option<fn(f64) -> f64>,
    /// Per-submobject stagger.
    pub lag_ratio: Option<f64>,
    /// Arc angle for the transform path.
    pub path_arc: Option<f64>,
    /// `(start, end)` within the play window.
    pub time_span: Option<(f64, f64)>,
}

impl PartialEq for AnimateArgs {
    fn eq(&self, other: &Self) -> bool {
        // Manual impl: rate functions compare by address (`fn_addr_eq`, the
        // sanctioned form), everything else by value.
        self.run_time == other.run_time
            && self.lag_ratio == other.lag_ratio
            && self.path_arc == other.path_arc
            && self.time_span == other.time_span
            && match (self.rate_func, other.rate_func) {
                (None, None) => true,
                (Some(a), Some(b)) => std::ptr::fn_addr_eq(a, b),
                _ => false,
            }
    }
}

/// A recorded positional mutation, applied to the target copy at build.
/// Every variant maps 1:1 onto the Stage positional surface (§8.4).
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum AnimateCommand {
    /// `shift(vector)`.
    Shift(Vec3),
    /// `scale(factor)` about the bounding-box center.
    Scale(f64),
    /// `stretch(factor, dim)`.
    Stretch(f64, usize),
    /// `center()`.
    Center,
    /// `move_to(target, aligned_edge)` — a Mob target resolves at build.
    MoveTo(PosTarget, Vec3),
    /// `next_to(target, direction, buff, aligned_edge)`.
    NextTo(PosTarget, Vec3, f64, Vec3),
    /// `align_to(target, direction)`.
    AlignTo(PosTarget, Vec3),
    /// `to_edge(edge, buff)`.
    ToEdge(Vec3, f64),
    /// `to_corner(corner, buff)`.
    ToCorner(Vec3, f64),
    /// `set_width(width, stretch)`.
    SetWidth(f64, bool),
    /// `set_height(height, stretch)`.
    SetHeight(f64, bool),
}

/// Marker for a W7 `override_animate` animation: the builder enforces the
/// no-chaining rule now; the actual overridden animations arrive with the
/// library classes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OverrideAnimation {
    /// The overriding animation's name (diagnostic identity until W7 binds
    /// real animation constructors here).
    pub name: &'static str,
}

/// A builder failure — each is one of the Reference's raise sites.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AnimateError {
    /// `set_anim_args` called twice on one chain.
    ArgsAlreadySet,
    /// An overridden animation was mixed with chained calls.
    OverrideNotChainable,
    /// A handle (the source, or a command's Mob target) is dead at build.
    StaleHandle(Mob),
}

impl std::fmt::Display for AnimateError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ArgsAlreadySet => write!(
                f,
                "animation arguments can only be passed by calling animate()/set_anim_args \
                 and can only be passed once"
            ),
            Self::OverrideNotChainable => write!(
                f,
                "method chaining is not supported for overridden animations"
            ),
            Self::StaleHandle(_) => write!(f, "animate target handle is stale"),
        }
    }
}

impl std::error::Error for AnimateError {}

/// The deferred-command recorder returned by [`Mob::animate`].
#[derive(Clone, Debug, PartialEq)]
pub struct AnimBuilder {
    source: Mob,
    commands: Vec<AnimateCommand>,
    args: Option<AnimateArgs>,
    overridden: Option<OverrideAnimation>,
    is_chaining: bool,
}

/// A realized recording: the source, the fully-positioned target copy, and
/// the animation arguments — what Choreo interpolates (fm-67a).
#[derive(Clone, Debug, PartialEq)]
pub struct BuiltAnimate {
    /// The animated mobject.
    pub source: Mob,
    /// The target copy the play interpolates toward (not in the scene).
    pub target: Mob,
    /// Transform-level arguments (defaults where unset).
    pub args: AnimateArgs,
    /// The overriding animation, if the chain recorded one.
    pub overridden: Option<OverrideAnimation>,
}

impl Mob {
    /// Start recording a deferred animation against this handle.
    #[must_use]
    pub fn animate(self) -> AnimBuilder {
        AnimBuilder {
            source: self,
            commands: Vec::new(),
            args: None,
            overridden: None,
            is_chaining: false,
        }
    }
}

impl AnimBuilder {
    /// The animated mobject.
    #[must_use]
    pub fn source(&self) -> Mob {
        self.source
    }

    /// The recorded commands, in order.
    #[must_use]
    pub fn commands(&self) -> &[AnimateCommand] {
        &self.commands
    }

    /// Pass the transform arguments — once per chain, as in the Reference.
    ///
    /// # Errors
    /// [`AnimateError::ArgsAlreadySet`] on a second call.
    pub fn set_anim_args(mut self, args: AnimateArgs) -> Result<Self, AnimateError> {
        if self.args.is_some() {
            return Err(AnimateError::ArgsAlreadySet);
        }
        self.args = Some(args);
        Ok(self)
    }

    /// Record an overridden animation. Neither preceded nor followed by
    /// chained calls, as in the Reference.
    ///
    /// # Errors
    /// [`AnimateError::OverrideNotChainable`].
    pub fn with_override(mut self, animation: OverrideAnimation) -> Result<Self, AnimateError> {
        if self.is_chaining || self.overridden.is_some() {
            return Err(AnimateError::OverrideNotChainable);
        }
        self.overridden = Some(animation);
        self.is_chaining = true;
        Ok(self)
    }

    fn push(mut self, command: AnimateCommand) -> Result<Self, AnimateError> {
        if self.overridden.is_some() {
            return Err(AnimateError::OverrideNotChainable);
        }
        self.is_chaining = true;
        self.commands.push(command);
        Ok(self)
    }

    /// Record `shift`.
    ///
    /// # Errors
    /// [`AnimateError::OverrideNotChainable`] after an override.
    pub fn shift(self, vector: Vec3) -> Result<Self, AnimateError> {
        self.push(AnimateCommand::Shift(vector))
    }

    /// Record `scale`.
    ///
    /// # Errors
    /// [`AnimateError::OverrideNotChainable`] after an override.
    pub fn scale(self, factor: f64) -> Result<Self, AnimateError> {
        self.push(AnimateCommand::Scale(factor))
    }

    /// Record `stretch`.
    ///
    /// # Errors
    /// [`AnimateError::OverrideNotChainable`] after an override.
    pub fn stretch(self, factor: f64, dim: usize) -> Result<Self, AnimateError> {
        self.push(AnimateCommand::Stretch(factor, dim))
    }

    /// Record `center`.
    ///
    /// # Errors
    /// [`AnimateError::OverrideNotChainable`] after an override.
    pub fn center(self) -> Result<Self, AnimateError> {
        self.push(AnimateCommand::Center)
    }

    /// Record `move_to`.
    ///
    /// # Errors
    /// [`AnimateError::OverrideNotChainable`] after an override.
    pub fn move_to(
        self,
        target: impl Into<PosTarget>,
        aligned_edge: Vec3,
    ) -> Result<Self, AnimateError> {
        self.push(AnimateCommand::MoveTo(target.into(), aligned_edge))
    }

    /// Record `next_to`.
    ///
    /// # Errors
    /// [`AnimateError::OverrideNotChainable`] after an override.
    pub fn next_to(
        self,
        target: impl Into<PosTarget>,
        direction: Vec3,
        buff: f64,
        aligned_edge: Vec3,
    ) -> Result<Self, AnimateError> {
        self.push(AnimateCommand::NextTo(
            target.into(),
            direction,
            buff,
            aligned_edge,
        ))
    }

    /// Record `align_to`.
    ///
    /// # Errors
    /// [`AnimateError::OverrideNotChainable`] after an override.
    pub fn align_to(
        self,
        target: impl Into<PosTarget>,
        direction: Vec3,
    ) -> Result<Self, AnimateError> {
        self.push(AnimateCommand::AlignTo(target.into(), direction))
    }

    /// Record `to_edge`.
    ///
    /// # Errors
    /// [`AnimateError::OverrideNotChainable`] after an override.
    pub fn to_edge(self, edge: Vec3, buff: f64) -> Result<Self, AnimateError> {
        self.push(AnimateCommand::ToEdge(edge, buff))
    }

    /// Record `to_corner`.
    ///
    /// # Errors
    /// [`AnimateError::OverrideNotChainable`] after an override.
    pub fn to_corner(self, corner: Vec3, buff: f64) -> Result<Self, AnimateError> {
        self.push(AnimateCommand::ToCorner(corner, buff))
    }

    /// Record `set_width`.
    ///
    /// # Errors
    /// [`AnimateError::OverrideNotChainable`] after an override.
    pub fn set_width(self, width: f64, stretch: bool) -> Result<Self, AnimateError> {
        self.push(AnimateCommand::SetWidth(width, stretch))
    }

    /// Record `set_height`.
    ///
    /// # Errors
    /// [`AnimateError::OverrideNotChainable`] after an override.
    pub fn set_height(self, height: f64, stretch: bool) -> Result<Self, AnimateError> {
        self.push(AnimateCommand::SetHeight(height, stretch))
    }

    /// Realize the recording: generate the target copy NOW (dynamic target
    /// lookup at build time), apply every command to it, and hand back the
    /// [`BuiltAnimate`] Choreo interpolates. The target is arena-allocated
    /// but not in the scene.
    ///
    /// # Errors
    /// [`AnimateError::StaleHandle`] for a dead source or a dead Mob target
    /// inside a command.
    pub fn build(self, stage: &mut Stage) -> Result<BuiltAnimate, AnimateError> {
        // Validate every Mob reference before mutating anything.
        if !stage.contains(self.source) {
            return Err(AnimateError::StaleHandle(self.source));
        }
        for command in &self.commands {
            if let AnimateCommand::MoveTo(PosTarget::Mob(m), _)
            | AnimateCommand::NextTo(PosTarget::Mob(m), _, _, _)
            | AnimateCommand::AlignTo(PosTarget::Mob(m), _) = command
                && !stage.contains(*m)
            {
                return Err(AnimateError::StaleHandle(*m));
            }
        }
        let target = stage
            .copy_family(self.source)
            .map_err(|_| AnimateError::StaleHandle(self.source))?;
        for command in &self.commands {
            apply(stage, target, *command);
        }
        Ok(BuiltAnimate {
            source: self.source,
            target,
            args: self.args.unwrap_or_default(),
            overridden: self.overridden,
        })
    }
}

fn apply(stage: &mut Stage, target: Mob, command: AnimateCommand) {
    match command {
        AnimateCommand::Shift(v) => {
            stage.shift(target, v);
        }
        AnimateCommand::Scale(f) => {
            stage.scale(target, f);
        }
        AnimateCommand::Stretch(f, dim) => {
            stage.stretch(target, f, dim);
        }
        AnimateCommand::Center => {
            stage.center(target);
        }
        AnimateCommand::MoveTo(t, aligned_edge) => {
            stage.move_to(target, t, aligned_edge);
        }
        AnimateCommand::NextTo(t, direction, buff, aligned_edge) => {
            stage.next_to(target, t, direction, buff, aligned_edge);
        }
        AnimateCommand::AlignTo(t, direction) => {
            stage.align_to(target, t, direction);
        }
        AnimateCommand::ToEdge(edge, buff) => {
            stage.to_edge(target, edge, buff);
        }
        AnimateCommand::ToCorner(corner, buff) => {
            stage.to_corner(target, corner, buff);
        }
        AnimateCommand::SetWidth(w, s) => {
            stage.set_width(target, w, s);
        }
        AnimateCommand::SetHeight(h, s) => {
            stage.set_height(target, h, s);
        }
    }
}

/// The `prepare_animation` contract (§9.1, shared with fmn-anim): a play
/// call accepts an animation or a builder — nothing else. Implemented for
/// [`AnimBuilder`] (which builds) and [`BuiltAnimate`] (identity); a bare
/// method reference is unrepresentable, which is the typed form of the
/// Reference's `TypeError`.
pub trait IntoAnimate {
    /// Convert into a realized animation input.
    ///
    /// # Errors
    /// [`AnimateError`] from the build.
    fn prepare(self, stage: &mut Stage) -> Result<BuiltAnimate, AnimateError>;
}

impl IntoAnimate for AnimBuilder {
    fn prepare(self, stage: &mut Stage) -> Result<BuiltAnimate, AnimateError> {
        self.build(stage)
    }
}

impl IntoAnimate for BuiltAnimate {
    fn prepare(self, stage: &mut Stage) -> Result<BuiltAnimate, AnimateError> {
        if stage.contains(self.source) && stage.contains(self.target) {
            Ok(self)
        } else {
            Err(AnimateError::StaleHandle(self.source))
        }
    }
}
