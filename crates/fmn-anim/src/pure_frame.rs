//! Callback-free native jobs for live, pure play and wait segments (§9.5).
//!
//! Purity alone does not make `dyn Animation` transferable. This module admits
//! only closed native mechanisms, freezes their **prepared** state, and invokes
//! its existing interpolation on a worker-local Stage. No begin, finish, target
//! recipe, updater or host callback is replayed there. Unknown mechanisms,
//! unknown rate functions, aliased endpoints and live views remain serial.

use std::collections::HashSet;
use std::rc::Rc;
use std::sync::Arc;

use fmn_core::rate::CatalogRate;
use fmn_core::rng::{Pcg64Dxsm, RngRoot};
use fmn_mobject::{FrozenRenderState, Mob, Snapshot, Stage};

use crate::animation::check_updater_error;
use crate::{
    AnimError, AnimState, Animation, FrameMetadata, FramePosition, FrameSample, FrameSegment,
    OpenSegment, OpenWait, RateFunc, RationalFrameClock, RationalTime, Rotating, SegmentKind,
    SegmentReport, ShowPartial, Transform, VFade, finish_open_play,
};

/// A closed, prepared interpolation mechanism. Fields and constructors are
/// private to Choreo: a custom callback cannot be wrapped in this type.
///
/// Transform's fade/grow/indication parameterizations, partial reveals,
/// vector-opacity fades, border/fill writing and absolute rotations reuse their
/// native mechanisms. Eager compositions recurse over those closed carriers.
/// Preparation and cleanup still run exactly once on the scene owner.
#[derive(Debug, Clone)]
pub struct FrozenAnimation {
    mechanism: FrozenMechanism,
}

// Keep this closed: a trait object would let a host callback or a stateful
// animation cross the owner boundary merely by claiming a pure signature.
#[derive(Debug, Clone)]
enum FrozenMechanism {
    Transform(Box<Transform>),
    Partial(ShowPartial),
    BorderFill(crate::DrawBorderThenFill),
    Subsets(crate::ShowIncreasingSubsets),
    CrossFade(crate::FadeTransform),
    Wiggle(crate::WiggleOutThenIn),
    StrokeFlash(crate::VShowPassingFlash),
    VectorFade(VFade),
    Rotation(Rotating),
    Group(FrozenGroup),
}

/// An eagerly begun composition, with the same interval table and timeline
/// mapping as its owner. Succession is deliberately excluded: later members
/// require lifecycle work and do not yet have prepared immutable endpoints.
#[derive(Debug, Clone)]
struct FrozenGroup {
    state: AnimState,
    animations: Vec<FrozenAnimation>,
    timings: Vec<crate::composition::Interval>,
    max_end_time: f64,
    frame_timings: bool,
    exact_timings: Option<Arc<crate::composition::ExactTimings>>,
}

impl FrozenGroup {
    fn interpolate_exact_position(
        &self,
        stage: &mut Stage,
        position: &FramePosition,
        timings: &crate::composition::ExactTimings,
        fps: u32,
    ) -> Result<(), AnimError> {
        for (index, animation) in self.animations.iter().enumerate() {
            if let Some(local) = timings.interior_frame(index, position)? {
                animation.interpolate_at_position(stage, &local, fps)?;
            } else {
                animation.interpolate_alpha(stage, timings.sub_alpha(index, position)?)?;
            }
        }
        Ok(())
    }
}

// Replace recognized Base rates with their data identity. A failed recognition
// is a safe demotion, never permission to approximate an arbitrary callback.
fn freeze_rate(rate: &RateFunc) -> Option<RateFunc> {
    Some(match rate {
        RateFunc::Catalog(rate) => RateFunc::Catalog(*rate),
        RateFunc::Base(function) => {
            RateFunc::Catalog(CatalogRate::from_tag(fmn_core::rate::tag_of(*function)?)?)
        }
        RateFunc::Sampled(samples) => RateFunc::Sampled(Arc::clone(samples)),
        RateFunc::Preparing(_) => return None,
        RateFunc::Exact(table) => RateFunc::Exact(Arc::clone(table)),
        RateFunc::Squish { inner, a, b } => RateFunc::Squish {
            inner: Box::new(freeze_rate(inner)?),
            a: *a,
            b: *b,
        },
        RateFunc::Scaled { inner, proportion } => RateFunc::Scaled {
            inner: Box::new(freeze_rate(inner)?),
            proportion: *proportion,
        },
    })
}

// A frozen exact table can still fail at an omitted intermediate sample.
// Catch-up must not hide that error by evaluating only a later valid sample.
// The ordinary frame-program route remains available for those tables.
fn rate_allows_owner_advance(rate: &RateFunc) -> bool {
    match rate {
        RateFunc::Catalog(_) => true,
        RateFunc::Base(_) | RateFunc::Preparing(_) | RateFunc::Exact(_) => false,
        RateFunc::Sampled(samples) => samples.iter().all(|sample| sample.is_finite()),
        RateFunc::Squish { inner, a, b } => {
            a.is_finite() && b.is_finite() && rate_allows_owner_advance(inner)
        }
        RateFunc::Scaled { inner, proportion } => {
            proportion.is_finite() && rate_allows_owner_advance(inner)
        }
    }
}

impl FrozenAnimation {
    fn prepare<A: Animation>(mut animation: A, row_width: usize) -> Option<A> {
        let state = animation.state();
        if state.starting_mobject().is_none()
            || state.families().iter().any(|row| row.len() != row_width)
        {
            return None;
        }
        let rate = freeze_rate(&state.config.rate_func)?;
        animation.state_mut().config.rate_func = rate;
        Some(animation)
    }

    pub(crate) fn transform(transform: Transform) -> Option<Self> {
        Some(Self {
            mechanism: FrozenMechanism::Transform(Box::new(Self::prepare(transform, 3)?)),
        })
    }

    pub(crate) fn partial(partial: ShowPartial) -> Option<Self> {
        Some(Self {
            mechanism: FrozenMechanism::Partial(Self::prepare(partial, 2)?),
        })
    }

    pub(crate) fn border_fill(animation: crate::DrawBorderThenFill) -> Option<Self> {
        Some(Self {
            mechanism: FrozenMechanism::BorderFill(Self::prepare(animation, 3)?),
        })
    }

    pub(crate) fn subsets(animation: crate::ShowIncreasingSubsets) -> Option<Self> {
        Some(Self {
            mechanism: FrozenMechanism::Subsets(Self::prepare(animation, 2)?),
        })
    }

    pub(crate) fn cross_fade(animation: crate::FadeTransform) -> Option<Self> {
        Some(Self {
            mechanism: FrozenMechanism::CrossFade(Self::prepare(animation, 3)?),
        })
    }

    pub(crate) fn wiggle(animation: crate::WiggleOutThenIn) -> Option<Self> {
        Some(Self {
            mechanism: FrozenMechanism::Wiggle(Self::prepare(animation, 2)?),
        })
    }

    pub(crate) fn stroke_flash(animation: crate::VShowPassingFlash) -> Option<Self> {
        Some(Self {
            mechanism: FrozenMechanism::StrokeFlash(Self::prepare(animation, 2)?),
        })
    }

    pub(crate) fn vector_fade(fade: VFade) -> Option<Self> {
        Some(Self {
            mechanism: FrozenMechanism::VectorFade(Self::prepare(fade, 2)?),
        })
    }

    pub(crate) fn rotation(rotation: Rotating) -> Option<Self> {
        Some(Self {
            mechanism: FrozenMechanism::Rotation(Self::prepare(rotation, 2)?),
        })
    }

    pub(crate) fn group(group: &crate::AnimationGroup) -> Option<Self> {
        let mut state = group.state().clone();
        state.config.rate_func = freeze_rate(&state.config.rate_func)?;
        let animations = group
            .animations()
            .iter()
            .map(|animation| animation.freeze_interpolation())
            .collect::<Option<Vec<_>>>()?;
        Some(Self {
            mechanism: FrozenMechanism::Group(FrozenGroup {
                state,
                animations,
                timings: group.interpolation_timings().to_vec(),
                max_end_time: group.interpolation_max_end(),
                frame_timings: group.uses_frame_timings(),
                exact_timings: group.exact_timings().cloned(),
            }),
        })
    }

    // Composition containers have no zipped families. Admission must inspect
    // every leaf, including aliases crossing a nested group boundary.
    fn leaf_states(&self) -> Vec<&AnimState> {
        match &self.mechanism {
            FrozenMechanism::Group(group) => group
                .animations
                .iter()
                .flat_map(Self::leaf_states)
                .collect(),
            _ => vec![self.state()],
        }
    }

    fn state(&self) -> &AnimState {
        match &self.mechanism {
            FrozenMechanism::Transform(animation) => animation.state(),
            FrozenMechanism::Partial(animation) => animation.state(),
            FrozenMechanism::BorderFill(animation) => animation.state(),
            FrozenMechanism::Subsets(animation) => animation.state(),
            FrozenMechanism::StrokeFlash(animation) => animation.state(),
            FrozenMechanism::Wiggle(animation) => animation.state(),
            FrozenMechanism::CrossFade(animation) => animation.state(),
            FrozenMechanism::VectorFade(animation) => animation.state(),
            FrozenMechanism::Rotation(animation) => animation.state(),
            FrozenMechanism::Group(group) => &group.state,
        }
    }

    fn rate_error(&self) -> Option<AnimError> {
        self.state().config.rate_func.error().or_else(|| {
            if let FrozenMechanism::Group(group) = &self.mechanism {
                group.animations.iter().find_map(Self::rate_error)
            } else {
                None
            }
        })
    }

    fn allows_owner_advance(&self) -> bool {
        rate_allows_owner_advance(&self.state().config.rate_func)
            && match &self.mechanism {
                // Exact composed frame arithmetic has sample-specific typed
                // failures. Keep its ordinary driver until a whole-interval
                // admission proof is available, just as for exact rate tables.
                FrozenMechanism::Group(group) => {
                    group.exact_timings.is_none()
                        && group.animations.iter().all(Self::allows_owner_advance)
                }
                _ => true,
            }
    }

    fn interpolate(&self, stage: &mut Stage, time: RationalTime) -> Result<(), AnimError> {
        if !matches!(&self.mechanism, FrozenMechanism::Group(_))
            && self.state().frame_units().is_none()
        {
            return self.interpolate_at_frame(stage, time.frames() as f64, time.fps());
        }
        self.interpolate_at_position(stage, &FramePosition::from_frame(time.frames()), time.fps())
    }

    fn interpolate_at_position(
        &self,
        stage: &mut Stage,
        position: &FramePosition,
        fps: u32,
    ) -> Result<(), AnimError> {
        if let FrozenMechanism::Group(group) = &self.mechanism {
            if let Some(timings) = &group.exact_timings {
                let position = timings.position(&group.state, position, fps)?;
                group.interpolate_exact_position(stage, &position, timings, fps)
            } else {
                self.interpolate_at_frame(stage, position.to_f64(), fps)
            }
        } else {
            self.interpolate_alpha(
                stage,
                self.state()
                    .frame_alpha_at_position(position, fps)
                    .map_err(AnimError::Clock)?,
            )
        }
    }

    fn interpolate_at_frame(
        &self,
        stage: &mut Stage,
        frame: f64,
        fps: u32,
    ) -> Result<(), AnimError> {
        if let FrozenMechanism::Group(group) = &self.mechanism {
            if group.exact_timings.is_some() {
                return self.interpolate_at_position(
                    stage,
                    &FramePosition::from_f64(frame).map_err(AnimError::Clock)?,
                    fps,
                );
            }
            let time = crate::composition::timeline_frame_position(
                &group.state,
                frame,
                fps,
                group.max_end_time,
            );
            for (animation, interval) in group.animations.iter().zip(&group.timings) {
                if group.frame_timings
                    && let Some(frame) = interval.interior_frame(time)
                {
                    animation.interpolate_at_frame(stage, frame, fps)?;
                } else {
                    animation.interpolate_alpha(stage, interval.sub_alpha(time))?;
                }
            }
        } else {
            self.interpolate_alpha(stage, self.state().frame_alpha(frame, fps))?;
        }
        Ok(())
    }

    fn interpolate_alpha(&self, stage: &mut Stage, alpha: f64) -> Result<(), AnimError> {
        // Clone only the prepared native carrier; never construct or begin it.
        // This intentionally calls the ordinary interpolation implementation,
        // including lag, time_span, locks, placements, uniforms and trackers.
        fn evaluate<A: Animation + Clone>(animation: &A, stage: &mut Stage, alpha: f64) {
            let mut animation = animation.clone();
            animation.interpolate(stage, alpha);
        }
        match &self.mechanism {
            // The box belongs to the frozen program. Clone only its prepared
            // carrier, preserving the existing per-frame scratch behavior.
            FrozenMechanism::Transform(animation) => evaluate(animation.as_ref(), stage, alpha),
            FrozenMechanism::Partial(animation) => evaluate(animation, stage, alpha),
            FrozenMechanism::BorderFill(animation) => evaluate(animation, stage, alpha),
            FrozenMechanism::Subsets(animation) => evaluate(animation, stage, alpha),
            FrozenMechanism::StrokeFlash(animation) => evaluate(animation, stage, alpha),
            FrozenMechanism::Wiggle(animation) => evaluate(animation, stage, alpha),
            FrozenMechanism::CrossFade(animation) => evaluate(animation, stage, alpha),
            FrozenMechanism::VectorFade(animation) => evaluate(animation, stage, alpha),
            FrozenMechanism::Rotation(animation) => evaluate(animation, stage, alpha),
            FrozenMechanism::Group(group) => {
                if let Some(timings) = &group.exact_timings {
                    let (position, fps) = crate::composition::exact_position_from_alpha(
                        &group.state,
                        alpha,
                        timings,
                    )?;
                    return group.interpolate_exact_position(stage, &position, timings, fps);
                }
                let time = crate::composition::timeline_position(
                    &group.state.config,
                    alpha,
                    group.max_end_time,
                );
                for (animation, interval) in group.animations.iter().zip(&group.timings) {
                    animation.interpolate_alpha(stage, interval.sub_alpha(time))?;
                }
            }
        }
        Ok(())
    }
}

/// An admission refusal is a request to keep the ordinary serial driver,
/// not a scene error and not a weakening of its existing purity report.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PureFrameRefusal {
    /// No unskipped pure begin snapshot, or the segment has already been stepped.
    NotAtPureBegin,
    /// A static wait is not at a completed pure boundary, or has no samples.
    NotAtPureWaitBoundary,
    /// The Stage/clock does not belong to the opened segment.
    DifferentBegin,
    /// This animation has no closed native interpolation carrier.
    UnsupportedAnimation(usize),
    /// A source, target or visible object has a live record view.
    LiveView(Mob),
    /// One animation writes an endpoint read by another (or itself).
    MutableEndpoint(Mob),
    /// Multiple animations write the same object; keep their serial semantics.
    OverlappingOutput(Mob),
}

impl std::fmt::Display for PureFrameRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotAtPureBegin => f.write_str("no unstepped pure begin state"),
            Self::NotAtPureWaitBoundary => f.write_str("no completed pure wait boundary"),
            Self::DifferentBegin => f.write_str("pure segment Stage or clock changed"),
            Self::UnsupportedAnimation(index) => {
                write!(f, "animation {index} has no callback-free native evaluator")
            }
            Self::LiveView(mob) => write!(f, "live record view on {mob:?}"),
            Self::MutableEndpoint(mob) => write!(f, "animation endpoint {mob:?} is animated"),
            Self::OverlappingOutput(mob) => write!(f, "multiple animations write {mob:?}"),
        }
    }
}

impl std::error::Error for PureFrameRefusal {}

fn freeze_animations(
    stage: &Stage,
    animations: &[Box<dyn Animation>],
) -> Result<Vec<FrozenAnimation>, PureFrameRefusal> {
    let mut frozen = Vec::with_capacity(animations.len());
    let mut writes = HashSet::new();
    let mut reads = Vec::new();
    let mut observed: Vec<Mob> = stage.roots().to_vec();
    for (index, animation) in animations.iter().enumerate() {
        let carrier = animation
            .freeze_interpolation()
            .ok_or(PureFrameRefusal::UnsupportedAnimation(index))?;
        for row in carrier
            .leaf_states()
            .into_iter()
            .flat_map(AnimState::families)
        {
            // Prepared rows remain interpolation inputs even if authoring
            // has since detached a member from its original family root.
            observed.extend(row.iter().copied());
            let Some((output, endpoints)) = row.split_first() else {
                return Err(PureFrameRefusal::UnsupportedAnimation(index));
            };
            if !writes.insert(*output) {
                return Err(PureFrameRefusal::OverlappingOutput(*output));
            }
            reads.extend(endpoints.iter().copied());
        }
        observed.extend(animation.all_mobjects());
        observed.extend(animation.preflight_mobjects());
        frozen.push(carrier);
    }
    for endpoint in reads {
        if writes.contains(&endpoint) {
            return Err(PureFrameRefusal::MutableEndpoint(endpoint));
        }
    }
    for root in observed {
        for mob in stage.family(root) {
            let entry = stage.get(mob).ok_or(PureFrameRefusal::DifferentBegin)?;
            if entry.buffer.live_view_count() != 0 {
                return Err(PureFrameRefusal::LiveView(mob));
            }
        }
    }
    Ok(frozen)
}

fn check_wait_live_views(stage: &Stage) -> Result<(), PureFrameRefusal> {
    for &root in stage.roots() {
        for mob in stage.family(root) {
            let entry = stage.get(mob).ok_or(PureFrameRefusal::DifferentBegin)?;
            if entry.buffer.live_view_count() != 0 {
                return Err(PureFrameRefusal::LiveView(mob));
            }
        }
    }
    Ok(())
}

#[derive(Debug, Clone, Copy)]
struct OwnerAdvancePlan {
    grid: FrameSegment,
    base_frame: i64,
    rng_seed: u64,
}

impl OwnerAdvancePlan {
    fn sample(
        &self,
        clock: &RationalFrameClock,
        rng: &RngRoot,
        report: &SegmentReport,
        stepped: i64,
        max_frames: u64,
    ) -> Result<Option<(FrameSample, RationalFrameClock)>, AnimError> {
        if !report.purity.is_pure() {
            return Ok(None);
        }
        let frames = self.grid.n_frames();
        let expected_frame = self.base_frame.checked_add(stepped);
        if report.base_frame != self.base_frame
            || report.n_frames != frames
            || expected_frame != Some(clock.now().frames())
            || clock.fps() != self.grid.end_time().fps()
            || rng.seed() != self.rng_seed
            || stepped < 0
            || stepped > frames
        {
            return Err(AnimError::InvalidFramePhase(
                "pure advance owner, clock or sampling plan changed",
            ));
        }
        let count = max_frames.min((frames - stepped).cast_unsigned());
        if count == 0 {
            return Ok(None);
        }
        let frame = stepped + count.cast_signed();
        let sample = self.grid.sample(frame).ok_or(AnimError::SeekOutOfRange {
            frame,
            total: frames,
        })?;
        // Check the whole clock advance before any owner mutation.
        let mut advanced_clock = *clock;
        advanced_clock
            .advance_frames(count.cast_signed())
            .map_err(AnimError::Clock)?;
        Ok(Some((sample, advanced_clock)))
    }
}

/// Forward advancement of a static wait on its already restored owner.
///
/// Unlike [`PureSegment`], this token cannot produce historical frame jobs.
/// Its snapshot binds only the logical Stage identity; it is never restored,
/// interpreted as the original begin state, or sent to a frame worker. The
/// existing report, original sampling grid and pending epilogue stay intact.
/// The caller must independently exclude stop conditions, external event
/// producers, per-frame captures and host-updater hooks from the skipped range.
pub struct PureWaitAdvance {
    plan: OwnerAdvancePlan,
    owner_anchor: Rc<Snapshot>,
    admitted_step: i64,
}

impl std::fmt::Debug for PureWaitAdvance {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PureWaitAdvance")
            .field("plan", &self.plan)
            .field("admitted_step", &self.admitted_step)
            .finish_non_exhaustive()
    }
}

impl PureWaitAdvance {
    /// Admit an open static wait at its current completed-frame boundary.
    /// A restored wait's matching Stage and clock must already be installed.
    /// This neither invokes callbacks nor supplies a missing begin snapshot.
    ///
    /// # Errors
    /// Historical/current impurity, live views, incomplete frame phases, an
    /// invalid clock/grid or an original begin owned by a different Stage.
    pub fn admit(
        stage: &Stage,
        clock: &RationalFrameClock,
        rng: &RngRoot,
        open: &OpenWait,
    ) -> Result<Self, PureFrameRefusal> {
        let report = open.report();
        if !open.is_at_frame_boundary()
            || stage.updater_error().is_some()
            || !report.purity.is_pure()
            || report.n_frames <= 0
            || !crate::classify_wait(stage, false).is_pure()
        {
            return Err(PureFrameRefusal::NotAtPureWaitBoundary);
        }
        open.checkpoint()
            .and_then(|checkpoint| checkpoint.validate_clock(clock))
            .map_err(|_| PureFrameRefusal::DifferentBegin)?;
        if report
            .begin_state
            .as_ref()
            .is_some_and(|begin| !stage.can_restore(begin))
        {
            return Err(PureFrameRefusal::DifferentBegin);
        }
        check_wait_live_views(stage)?;
        Ok(Self {
            plan: OwnerAdvancePlan {
                grid: open.segment(),
                base_frame: report.base_frame,
                rng_seed: rng.seed(),
            },
            owner_anchor: report
                .begin_state
                .as_ref()
                .map_or_else(|| Rc::new(stage.snapshot()), Rc::clone),
            admitted_step: open.stepped(),
        })
    }

    /// Advance only future unobserved samples, leaving the wait open.
    /// Current purity, views, exact clock, root RNG and owner identity are
    /// rechecked before mutation. Stateful history always retains serial work.
    ///
    /// # Errors
    /// A changed owner/grid/clock, a cursor before admission, an incomplete
    /// frame or an unacknowledged updater failure. A serial fallback is `None`.
    pub fn try_advance(
        &self,
        stage: &mut Stage,
        clock: &mut RationalFrameClock,
        rng: &RngRoot,
        open: &mut OpenWait,
        max_frames: u64,
    ) -> Result<Option<FrameMetadata>, AnimError> {
        check_updater_error(stage)?;
        if open.segment() != self.plan.grid || !open.is_at_frame_boundary() {
            return Err(AnimError::InvalidFramePhase(
                "pure advance requires the admitted wait at a completed frame boundary",
            ));
        }
        if !open.report().purity.is_pure() {
            return Ok(None);
        }
        if !stage.can_restore(&self.owner_anchor) || open.stepped() < self.admitted_step {
            return Err(AnimError::InvalidFramePhase(
                "pure wait owner or admission boundary changed",
            ));
        }
        let Some((sample, advanced_clock)) =
            self.plan
                .sample(clock, rng, open.report(), open.stepped(), max_frames)?
        else {
            return Ok(None);
        };
        if !crate::classify_wait(stage, false).is_pure() || check_wait_live_views(stage).is_err() {
            return Ok(None);
        }
        *clock = advanced_clock;
        stage.set_time_from_clock(clock.now().to_f64());
        open.advance_pure_cursor(sample.frame);
        Ok(Some(FrameMetadata::capture(clock, rng, &sample)))
    }
}

struct SegmentData {
    kind: SegmentKind,
    begin: FrozenRenderState,
    animations: Vec<FrozenAnimation>,
    grid: FrameSegment,
    base_frame: i64,
    rng_seed: u64,
}

/// An immutable native program shared by all of its bounded frame jobs.
///
/// Construct immediately after `open_play` or `open_wait`, before any frame is
/// stepped or authored state is changed. The caller must separately rule out external
/// event listeners, source inboxes and per-frame host hooks. The normal Scene
/// integration must do this and sinks must opt in; recording/interactive sinks
/// continue to receive every ordinary FramePacket.
#[derive(Clone)]
pub struct PureSegment {
    data: Arc<SegmentData>,
}

impl std::fmt::Debug for PureSegment {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PureSegment")
            .field("kind", &self.data.kind)
            .field("base_frame", &self.data.base_frame)
            .field("frames", &self.frame_count())
            .field("animations", &self.data.animations.len())
            .finish_non_exhaustive()
    }
}

impl PureSegment {
    /// Images retained by the prepared begin state, before any offloaded frame
    /// can publish. Input-capability checks must run on this complete set.
    pub fn image_resources(&self) -> impl Iterator<Item = &fmn_mobject::ImageResource> {
        self.data.begin.image_resources()
    }

    /// Admit and freeze the already-begun native animations without retaining
    /// an Rc, updater, view or arbitrary callable in the worker representation.
    ///
    /// # Errors
    /// A [`PureFrameRefusal`] means the caller must use ordinary stepped play.
    pub fn freeze(
        stage: &Stage,
        clock: &RationalFrameClock,
        rng: &RngRoot,
        animations: &[Box<dyn Animation>],
        open: &OpenSegment,
    ) -> Result<Self, PureFrameRefusal> {
        let report = open.report();
        let begin = report
            .begin_state
            .as_ref()
            .filter(|_| {
                report.purity.is_pure()
                    && stage.updater_error().is_none()
                    && open.is_at_begin()
                    && report.n_frames > 0
                    && crate::classify_play(stage, animations).is_pure()
            })
            .ok_or(PureFrameRefusal::NotAtPureBegin)?;
        if !stage.can_restore(begin)
            || clock.now().frames() != report.base_frame
            || clock.fps() != open.segment().end_time().fps()
        {
            return Err(PureFrameRefusal::DifferentBegin);
        }
        let frozen = freeze_animations(stage, animations)?;
        Ok(Self {
            data: Arc::new(SegmentData {
                kind: SegmentKind::Play,
                begin: begin.render_state(clock.now().to_f64()),
                animations: frozen,
                grid: open.segment(),
                base_frame: report.base_frame,
                rng_seed: rng.seed(),
            }),
        })
    }

    /// Freeze a static wait after its one initial zero-dt updater pass.
    /// A wait with updaters, a live view or an external host-updater boundary
    /// remains serial. Stop-condition waits must never enter this API.
    ///
    /// # Errors
    /// A refusal retains the already-opened wait's ordinary stepped driver.
    pub fn freeze_wait(
        stage: &Stage,
        clock: &RationalFrameClock,
        rng: &RngRoot,
        open: &OpenWait,
    ) -> Result<Self, PureFrameRefusal> {
        let report = open.report();
        let begin = report
            .begin_state
            .as_ref()
            .filter(|_| {
                open.is_at_begin()
                    && stage.updater_error().is_none()
                    && report.purity.is_pure()
                    && report.n_frames > 0
                    && crate::classify_wait(stage, false).is_pure()
            })
            .ok_or(PureFrameRefusal::NotAtPureBegin)?;
        if !stage.can_restore(begin)
            || clock.now().frames() != report.base_frame
            || clock.fps() != open.segment().end_time().fps()
        {
            return Err(PureFrameRefusal::DifferentBegin);
        }
        check_wait_live_views(stage)?;
        Ok(Self {
            data: Arc::new(SegmentData {
                kind: SegmentKind::Wait,
                begin: begin.render_state(clock.now().to_f64()),
                animations: Vec::new(),
                grid: open.segment(),
                base_frame: report.base_frame,
                rng_seed: rng.seed(),
            }),
        })
    }

    /// Advance an admitted play's original owner over unobserved pure frames.
    ///
    /// Apply the last requested nominal sample once, leave the animation
    /// epilogue pending, and retain the cursor for either another pure advance
    /// or ordinary stepped continuation. This never restores a worker Stage
    /// over live owner registrations. The caller must independently exclude
    /// per-frame observers, queued events and concurrent event producers.
    ///
    /// `None` leaves the owner unchanged: the count is zero, the segment is
    /// complete, or current callbacks/views/dependencies require serial work.
    /// Exact rate tables and exact composed frame arithmetic remain serial so
    /// an error at an intermediate sample cannot be hidden by a later sample.
    ///
    /// # Errors
    /// A mismatched owner/cursor/clock, an incomplete frame phase, or a native
    /// interpolation failure. As in stepped playback, an interpolation failure
    /// can leave partial geometry and requires the caller's ordinary abort path.
    pub fn try_advance_on_owner(
        &self,
        stage: &mut Stage,
        clock: &mut RationalFrameClock,
        rng: &RngRoot,
        animations: &mut [Box<dyn Animation>],
        open: &mut OpenSegment,
        max_frames: u64,
    ) -> Result<Option<FrameMetadata>, AnimError> {
        check_updater_error(stage)?;
        if self.data.kind != SegmentKind::Play
            || animations.len() != self.data.animations.len()
            || open.segment() != self.data.grid
            || !open.is_at_frame_boundary()
        {
            return Err(AnimError::InvalidFramePhase(
                "pure advance requires the original play at a completed frame boundary",
            ));
        }
        if let Some(error) = animations
            .iter()
            .find_map(|animation| animation.deferred_error())
        {
            return Err(error);
        }
        let Some((sample, advanced_clock)) = self.owner_advance_sample(
            stage,
            clock,
            rng,
            open.report(),
            open.stepped(),
            max_frames,
        )?
        else {
            return Ok(None);
        };
        if !crate::classify_play(stage, animations).is_pure() {
            return Ok(None);
        }
        // A caller may have attached a view/updater or changed a rate since
        // freezing. Recheck the current native carriers, including aliases
        // across nested eager compositions, before touching owner state.
        let Ok(frozen) = freeze_animations(stage, animations) else {
            return Ok(None);
        };
        if !frozen.iter().all(FrozenAnimation::allows_owner_advance) {
            return Ok(None);
        }
        for animation in animations.iter_mut() {
            animation.interpolate_at_time(stage, sample.time)?;
            check_updater_error(stage)?;
        }
        if let Some(error) = animations
            .iter()
            .find_map(|animation| animation.deferred_error())
        {
            return Err(error);
        }
        *clock = advanced_clock;
        stage.set_time_from_clock(clock.now().to_f64());
        open.advance_pure_cursor(sample.frame);
        Ok(Some(FrameMetadata::capture(clock, rng, &sample)))
    }

    /// Advance an admitted static wait without inventing an updater pass.
    /// The initial zero-dt pass already ran when the wait opened; current
    /// purity and live-view checks prove the omitted per-frame passes inert.
    /// The wait remains open, including at its final nominal sample.
    ///
    /// # Errors
    /// A mismatched owner/cursor/clock, an incomplete frame phase, or a retained
    /// updater error. A refused admission returns `None` without mutation.
    pub fn try_advance_wait_on_owner(
        &self,
        stage: &mut Stage,
        clock: &mut RationalFrameClock,
        rng: &RngRoot,
        open: &mut OpenWait,
        max_frames: u64,
    ) -> Result<Option<FrameMetadata>, AnimError> {
        check_updater_error(stage)?;
        if self.data.kind != SegmentKind::Wait
            || open.segment() != self.data.grid
            || !open.is_at_frame_boundary()
        {
            return Err(AnimError::InvalidFramePhase(
                "pure advance requires the original wait at a completed frame boundary",
            ));
        }
        let Some((sample, advanced_clock)) = self.owner_advance_sample(
            stage,
            clock,
            rng,
            open.report(),
            open.stepped(),
            max_frames,
        )?
        else {
            return Ok(None);
        };
        if !crate::classify_wait(stage, false).is_pure() || check_wait_live_views(stage).is_err() {
            return Ok(None);
        }
        *clock = advanced_clock;
        stage.set_time_from_clock(clock.now().to_f64());
        open.advance_pure_cursor(sample.frame);
        Ok(Some(FrameMetadata::capture(clock, rng, &sample)))
    }

    fn owner_advance_sample(
        &self,
        stage: &Stage,
        clock: &RationalFrameClock,
        rng: &RngRoot,
        report: &SegmentReport,
        stepped: i64,
        max_frames: u64,
    ) -> Result<Option<(FrameSample, RationalFrameClock)>, AnimError> {
        // A completed external-updater boundary deliberately drops the begin
        // snapshot and demotes the report. That is a serial fallback, not a
        // malformed owner whose clock should be repaired or rejected here.
        if !report.purity.is_pure() {
            return Ok(None);
        }
        if report
            .begin_state
            .as_ref()
            .is_none_or(|begin| !stage.can_restore(begin))
        {
            return Err(AnimError::InvalidFramePhase(
                "pure advance owner, clock or sampling plan changed",
            ));
        }
        OwnerAdvancePlan {
            grid: self.data.grid,
            base_frame: self.data.base_frame,
            rng_seed: self.data.rng_seed,
        }
        .sample(clock, rng, report, stepped, max_frames)
    }

    /// Advance the original wait owner once after consuming its immutable jobs.
    /// No final updater pass is invented: ordinary waits have only the initial
    /// zero-dt pass and the per-sample passes that purity proved were inert.
    ///
    /// # Errors
    /// A different program kind, opened wait or clock is refused before advance.
    pub fn finish_wait_on_owner(
        &self,
        stage: &mut Stage,
        clock: &mut RationalFrameClock,
        open: OpenWait,
    ) -> Result<SegmentReport, AnimError> {
        check_updater_error(stage)?;
        if self.data.kind != SegmentKind::Wait
            || !open.is_at_begin()
            || !open.report().purity.is_pure()
            || open.report().base_frame != self.data.base_frame
            || open.report().n_frames != self.frame_count()
            || clock.now().frames() != self.data.base_frame
            || clock.fps() != self.data.grid.end_time().fps()
            || open
                .report()
                .begin_state
                .as_ref()
                .is_none_or(|begin| !stage.can_restore(begin))
        {
            return Err(AnimError::InvalidFramePhase(
                "pure wait no longer at its begin boundary",
            ));
        }
        clock
            .advance_frames(self.frame_count())
            .map_err(AnimError::Clock)?;
        stage.set_time_from_clock(clock.now().to_f64());
        open.finish_pure()
    }

    /// Number of nominal samples; no alpha-zero frame is invented.
    #[must_use]
    pub fn frame_count(&self) -> i64 {
        self.data.grid.n_frames()
    }

    /// Create a constant-size frame job; geometry is shared, not captured again.
    ///
    /// # Errors
    /// [`AnimError::SeekOutOfRange`] outside this segment's 1-based frame range.
    pub fn job(&self, frame: i64) -> Result<PureFrameJob, AnimError> {
        let sample = self
            .data
            .grid
            .sample(frame)
            .ok_or(AnimError::SeekOutOfRange {
                frame,
                total: self.frame_count(),
            })?;
        Ok(PureFrameJob {
            data: Arc::clone(&self.data),
            sample,
        })
    }

    /// Complete an admitted play on its original owner, without restoring a
    /// callback-free worker projection over the owner's live registrations.
    ///
    /// Call once, even after sink failure (ordinary play likewise completes
    /// its front end after a capture failure). No per-frame observer may run
    /// on this route. The last nominal sample is applied before the ordinary
    /// finish/remover/resume/zero-dt epilogue; final_alpha_value is not a frame.
    ///
    /// # Errors
    /// A mismatched open/clock, or ordinary animation cleanup failure.
    pub fn finish_on_owner(
        &self,
        stage: &mut Stage,
        clock: &mut RationalFrameClock,
        animations: &mut [Box<dyn Animation>],
        open: OpenSegment,
    ) -> Result<SegmentReport, AnimError> {
        if let Err(error) = check_updater_error(stage) {
            let _report = crate::frame::abort_open_play(stage, animations, open);
            return Err(error);
        }
        if self.data.kind != SegmentKind::Play
            || !open.is_at_begin()
            || animations.len() != self.data.animations.len()
            || open.report().base_frame != self.data.base_frame
            || open.report().n_frames != self.frame_count()
            || clock.now().frames() != self.data.base_frame
            || clock.fps() != self.data.grid.end_time().fps()
        {
            return Err(AnimError::InvalidFramePhase(
                "pure play no longer at its begin boundary",
            ));
        }
        let last = self.data.grid.end_time();
        let final_progression = if self.frame_count() == 0 {
            0
        } else {
            animations.len()
        };
        for index in 0..final_progression {
            let animation = &mut animations[index];
            if let Err(error) = animation
                .interpolate_at_time(stage, last)
                .and_then(|()| check_updater_error(stage))
            {
                let _report = crate::frame::abort_open_play(stage, animations, open);
                return Err(error);
            }
        }
        clock
            .advance_frames(self.frame_count())
            .map_err(AnimError::Clock)?;
        stage.set_time_from_clock(clock.now().to_f64());
        finish_open_play(stage, animations, open)
    }
}

/// One sample of a live native pure segment. Safe to execute out of order on any
/// worker; both mutable animation carriers and the materialized Stage are local.
#[derive(Clone)]
pub struct PureFrameJob {
    data: Arc<SegmentData>,
    sample: FrameSample,
}

impl std::fmt::Debug for PureFrameJob {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PureFrameJob")
            .field("time", &self.time())
            .finish_non_exhaustive()
    }
}

impl PureFrameJob {
    /// Exact global scene time, independent of output sequence rebasing.
    #[must_use]
    pub fn time(&self) -> RationalTime {
        self.sample.time + self.data.base_frame
    }

    /// The original sample, including the segment-local time and alpha.
    #[must_use]
    pub fn sample(&self) -> FrameSample {
        self.sample
    }

    /// The same named frame-indexed RNG fork supplied by FramePacket.
    #[must_use]
    pub fn rng_fork(&self, substream: &str) -> Pcg64Dxsm {
        RngRoot::from_seed(self.data.rng_seed)
            .substream(substream)
            .fork_frame(self.time().frames().cast_unsigned())
    }

    /// Reconstruct using Choreo's native interpolation, not whole-scene lerp
    /// between guessed endpoints. In particular, arcs, lagged families and
    /// non-monotone catalog rates keep their real intermediate states.
    ///
    /// # Panics
    /// If an exact rate table does not cover this sample. Use
    /// [`Self::try_materialize`] to handle that failure as an error.
    #[must_use]
    pub fn materialize(&self) -> Stage {
        self.try_materialize()
            .expect("pure frame interpolation failed")
    }

    /// Reconstruct with a typed failure when an exact rate table does not
    /// cover the requested sample. Production renderers use this checked
    /// boundary, so no partially interpolated frame can reach an emitter.
    ///
    /// # Errors
    /// An exact rate's preparation contract was violated.
    pub fn try_materialize(&self) -> Result<Stage, AnimError> {
        let mut stage = self.data.begin.clone().materialize();
        for animation in &self.data.animations {
            animation.interpolate(&mut stage, self.sample.time)?;
            if let Some(error) = animation.rate_error() {
                return Err(error);
            }
        }
        stage.set_time_from_clock(self.time().to_f64());
        Ok(stage)
    }
}

/// An opt-in consumer for closed native frame programs. Ordinary consumers
/// implement only `capture`; their per-frame observations remain serial.
pub trait PlayCapture {
    /// Consume an ordinary owner-thread capture.
    fn capture(&mut self, packet: crate::FramePacket);

    /// Whether the next ordinary capture needs an immutable scene snapshot.
    ///
    /// The default preserves existing full-packet consumers. Opting out only
    /// elides freezing: every animation, updater, clock and boundary still runs.
    /// This is checked after the boundary for each individual frame.
    fn capture_state_required(&self) -> bool {
        true
    }

    /// Observe one capture for which [`Self::capture_state_required`] was false.
    fn capture_metadata(&mut self, _metadata: crate::FrameMetadata) {}

    /// Whether preparing a compact program could be useful to this consumer.
    fn accepts_pure_segments(&self) -> bool {
        false
    }

    /// Consume a complete immutable program instead of individual captures.
    /// Return false only without having consumed or published any part of it;
    /// the driver then continues the ALREADY begun play serially.
    fn capture_pure_segment(&mut self, _segment: &PureSegment) -> bool {
        false
    }
}

/// Drive a native play with optional pure-segment evaluation on workers.
///
/// `allow_pure` is the caller's proof that no boundary callback, external
/// event source or host hook can observe intermediate owner-thread states.
/// A sink must separately opt in. Admission refusals preserve ordinary play,
/// including its one-time begin and its failure/finish/cleanup semantics.
/// Skipped plays always run the original stepped driver without captures.
///
/// # Errors
/// Ordinary begin, clock or deferred animation failures.
#[allow(clippy::too_many_arguments)]
pub fn play_segment_with_pure_dispatch(
    stage: &mut Stage,
    clock: &mut RationalFrameClock,
    rng: &RngRoot,
    animations: &mut [Box<dyn Animation>],
    skip: bool,
    allow_pure: bool,
    boundary: &mut dyn FnMut(&mut Stage, RationalTime),
    capture: &mut dyn PlayCapture,
) -> Result<SegmentReport, AnimError> {
    // The begin snapshot is the pure program's input and nothing else reads
    // it here, so decide whether one can be offered before copying the arena.
    let offer = allow_pure && !skip && capture.accepts_pure_segments();
    let open =
        crate::open_play_with_mode(stage, clock, animations, crate::BeginState::retained(offer))?;
    if offer
        && let Ok(segment) = PureSegment::freeze(stage, clock, rng, animations, &open)
        && capture.capture_pure_segment(&segment)
    {
        return segment.finish_on_owner(stage, clock, animations, open);
    }
    crate::frame::drive_open_play(stage, clock, rng, animations, skip, boundary, capture, open)
}

/// Drive a wait with the same frame jobs as pure plays. The caller must exclude
/// observers and stop conditions; stateful waits and admission refusals retain
/// every nominal updater/capture boundary. Sink refusal never repeats the
/// already-completed initial zero-dt updater pass.
///
/// # Errors
/// Ordinary duration, clock or frame-phase failures.
#[allow(clippy::too_many_arguments)]
pub fn wait_segment_with_pure_dispatch(
    stage: &mut Stage,
    clock: &mut RationalFrameClock,
    rng: &RngRoot,
    duration: impl Into<crate::Duration>,
    skip: bool,
    allow_pure: bool,
    boundary: &mut dyn FnMut(&mut Stage, RationalTime),
    capture: &mut dyn PlayCapture,
) -> Result<SegmentReport, AnimError> {
    let offer = allow_pure && !skip && capture.accepts_pure_segments();
    let mut open = crate::open_wait(stage, clock, duration, crate::BeginState::retained(offer))?;
    if offer
        && let Ok(segment) = PureSegment::freeze_wait(stage, clock, rng, &open)
        && capture.capture_pure_segment(&segment)
    {
        return segment.finish_wait_on_owner(stage, clock, open);
    }
    while crate::prepare_wait_frame(stage, clock, &mut open)?.is_some() {
        crate::complete_wait_frame_with_capture(
            stage, clock, rng, &mut open, !skip, boundary, capture,
        )?;
    }
    crate::finish_open_wait(open)
}

#[cfg(test)]
mod wait_advance_tests {
    use super::*;
    use crate::{Duration, OpenWaitCheckpoint, Purity, finish_open_wait, resume_open_wait};
    use fmn_mobject::Mobject;

    #[test]
    fn resumed_wait_advance_preserves_exact_large_and_seconds_grids_without_begin_state() {
        let large = 1_i64 << 53;
        for (duration, base, stepped, target, alpha) in [
            (
                Duration::Frames((large + 1).cast_unsigned()),
                0,
                7,
                large,
                1.0_f64.next_down(),
            ),
            (
                Duration::Frames(i64::MAX.cast_unsigned()),
                0,
                i64::MAX - 3,
                i64::MAX - 1,
                1.0_f64.next_down(),
            ),
            (Duration::Seconds(43.0 / 24.0), 3, 4, 43, 1.0),
        ] {
            let mut stage = Stage::new();
            let mob = stage.add(Mobject::from_points(&[[0.0; 3]]));
            stage.add_to_scene(mob).unwrap();
            let mut clock = RationalFrameClock::new(24).unwrap();
            let segment = clock.segment(duration).unwrap();
            clock.advance_frames(base + stepped).unwrap();
            let rng = RngRoot::from_seed(42);
            let mut open = resume_open_wait(
                &stage,
                &clock,
                OpenWaitCheckpoint {
                    segment,
                    base_frame: base,
                    stepped,
                    purity: Purity::Pure,
                },
            )
            .unwrap();
            let advance = PureWaitAdvance::admit(&stage, &clock, &rng, &open).unwrap();
            assert_eq!(
                PureSegment::freeze_wait(&stage, &clock, &rng, &open).unwrap_err(),
                PureFrameRefusal::NotAtPureBegin,
                "a forward token must not invent a full-history frame program",
            );
            let metadata = advance
                .try_advance(
                    &mut stage,
                    &mut clock,
                    &rng,
                    &mut open,
                    (target - stepped).cast_unsigned(),
                )
                .unwrap()
                .unwrap();
            assert_eq!(metadata.segment_frame(), target);
            assert_eq!(metadata.frame_index(), base + target);
            assert_eq!(metadata.alpha().to_bits(), alpha.to_bits());
            assert_eq!(
                metadata.rng_fork("resume").next_u64(),
                rng.substream("resume")
                    .fork_frame((base + target).cast_unsigned())
                    .next_u64(),
            );
            assert!(open.report().begin_state.is_none());
            assert!(!open.frames_complete());
            advance
                .try_advance(&mut stage, &mut clock, &rng, &mut open, u64::MAX)
                .unwrap()
                .unwrap();
            assert!(open.frames_complete());
            assert_eq!(clock.now().frames(), base + segment.n_frames());
            let report = finish_open_wait(open).unwrap();
            assert!(report.begin_state.is_none());
            assert!(report.purity.is_pure());
        }
    }

    #[test]
    fn resumed_wait_advance_refuses_foreign_owner_rng_clock_and_rewound_cursor() {
        let mut stage = Stage::new();
        let mob = stage.add(Mobject::from_points(&[[0.0; 3]]));
        stage.add_to_scene(mob).unwrap();
        let mut clock = RationalFrameClock::new(24).unwrap();
        clock.advance_frames(7).unwrap();
        let checkpoint = OpenWaitCheckpoint {
            segment: clock.segment(Duration::Frames(24)).unwrap(),
            base_frame: 0,
            stepped: 7,
            purity: Purity::Pure,
        };
        let rng = RngRoot::from_seed(42);
        let mut open = resume_open_wait(&stage, &clock, checkpoint.clone()).unwrap();
        let advance = PureWaitAdvance::admit(&stage, &clock, &rng, &open).unwrap();
        let before = stage.snapshot_bytes().unwrap();
        let mut foreign = Stage::new();
        let mut foreign_clock = clock;
        assert!(
            advance
                .try_advance(&mut foreign, &mut foreign_clock, &rng, &mut open, 3)
                .is_err()
        );
        assert_eq!(foreign_clock.now(), clock.now());
        assert_eq!(open.checkpoint().unwrap(), checkpoint);

        let different_rng = RngRoot::from_seed(43);
        assert!(
            advance
                .try_advance(&mut stage, &mut clock, &different_rng, &mut open, 3)
                .is_err()
        );
        let mut wrong_clock = clock;
        wrong_clock.advance_frames(1).unwrap();
        assert!(
            advance
                .try_advance(&mut stage, &mut wrong_clock, &rng, &mut open, 3)
                .is_err()
        );
        assert_eq!(wrong_clock.now().frames(), 8);

        let mut rewound_clock = RationalFrameClock::new(24).unwrap();
        rewound_clock.advance_frames(6).unwrap();
        let mut rewound = resume_open_wait(
            &stage,
            &rewound_clock,
            OpenWaitCheckpoint {
                stepped: 6,
                ..checkpoint.clone()
            },
        )
        .unwrap();
        assert!(
            advance
                .try_advance(&mut stage, &mut rewound_clock, &rng, &mut rewound, 3)
                .is_err()
        );
        assert_eq!(rewound_clock.now().frames(), 6);
        assert_eq!(rewound.checkpoint().unwrap().stepped, 6);
        assert_eq!(clock.now().frames(), 7);
        assert_eq!(open.checkpoint().unwrap(), checkpoint);
        assert_eq!(stage.snapshot_bytes().unwrap(), before);
    }
}
