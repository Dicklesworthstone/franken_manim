//! Callback-free native interpolation jobs for live, pure play segments (§9.5).
//!
//! Purity alone does not make `dyn Animation` transferable. This module admits
//! only a closed native mechanism, freezes its **prepared** state, and invokes
//! its existing interpolation on a worker-local Stage. No begin, finish, target
//! recipe, updater or host callback is replayed there. Unknown mechanisms,
//! unknown rate functions, aliased endpoints and live views remain serial.

use std::collections::HashSet;
use std::sync::Arc;

use fmn_core::rate::CatalogRate;
use fmn_core::rng::{Pcg64Dxsm, RngRoot};
use fmn_mobject::{FrozenRenderState, Mob, Stage};

use crate::{
    AnimError, Animation, FrameSample, FrameSegment, OpenSegment, RateFunc,
    RationalFrameClock, RationalTime, SegmentReport, Transform, finish_open_play,
};

/// A closed, prepared interpolation mechanism. Fields and constructors are
/// private to Choreo: a custom callback cannot be wrapped in this type.
///
/// Transform's fade/grow/indication parameterizations use the same mechanism;
/// their preparation and cleanup still run exactly once on the scene owner.
#[derive(Debug, Clone)]
pub struct FrozenAnimation {
    transform: Transform,
}

// Replace recognized Base rates with their data identity. A failed recognition
// is a safe demotion, never permission to approximate an arbitrary callback.
fn freeze_rate(rate: &RateFunc) -> Option<RateFunc> {
    Some(match rate {
        RateFunc::Catalog(rate) => RateFunc::Catalog(*rate),
        RateFunc::Base(function) => RateFunc::Catalog(CatalogRate::from_tag(
            fmn_core::rate::tag_of(*function)?,
        )?),
        RateFunc::Sampled(samples) => RateFunc::Sampled(Arc::clone(samples)),
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

impl FrozenAnimation {
    pub(crate) fn transform(mut transform: Transform) -> Option<Self> {
        let state = transform.state();
        if state.starting_mobject().is_none()
            || state.families().iter().any(|row| row.len() != 3)
        {
            return None;
        }
        let rate = freeze_rate(&state.config.rate_func)?;
        transform.state_mut().config.rate_func = rate;
        Some(Self { transform })
    }

    fn interpolate(&self, stage: &mut Stage, time: f64) {
        // Clone only the prepared native carrier; never construct or begin it.
        // This intentionally calls the ordinary interpolation implementation,
        // including lag, time_span, locks, placements, uniforms and trackers.
        let mut animation = self.transform.clone();
        let alpha = time / animation.state().config.run_time;
        animation.interpolate(stage, alpha);
    }
}

/// An admission refusal is a request to keep the ordinary serial driver,
/// not a scene error and not a weakening of its existing purity report.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PureFrameRefusal {
    /// No unskipped pure begin snapshot, or the play has already been stepped.
    NotAtPureBegin,
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

struct SegmentData {
    begin: FrozenRenderState,
    animations: Vec<FrozenAnimation>,
    grid: FrameSegment,
    base_frame: i64,
    rng_seed: u64,
}

/// An immutable native play program shared by all of its bounded frame jobs.
///
/// Construct immediately after `open_play`, before any frame is stepped or
/// authored state is changed. The caller must separately rule out external
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
            .field("base_frame", &self.data.base_frame)
            .field("frames", &self.frame_count())
            .field("animations", &self.data.animations.len())
            .finish_non_exhaustive()
    }
}

impl PureSegment {
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
        let begin = report.begin_state.as_ref().filter(|_| {
            report.purity.is_pure() && open.is_at_begin() && report.n_frames > 0
                && crate::classify_play(stage, animations).is_pure()
        }).ok_or(PureFrameRefusal::NotAtPureBegin)?;
        if !stage.can_restore(begin)
            || clock.now().frames() != report.base_frame
            || clock.fps() != open.segment().end_time().fps()
        {
            return Err(PureFrameRefusal::DifferentBegin);
        }
        let mut frozen = Vec::with_capacity(animations.len());
        let mut writes = HashSet::new();
        let mut reads = Vec::new();
        let mut observed: Vec<Mob> = stage.roots().to_vec();
        for (index, animation) in animations.iter().enumerate() {
            let carrier = animation.freeze_interpolation()
                .ok_or(PureFrameRefusal::UnsupportedAnimation(index))?;
            for row in carrier.transform.state().families() {
                let [output, start, target] = row.as_slice() else {
                    return Err(PureFrameRefusal::UnsupportedAnimation(index));
                };
                if !writes.insert(*output) {
                    return Err(PureFrameRefusal::OverlappingOutput(*output));
                }
                reads.extend([*start, *target]);
            }
            observed.extend(animation.all_mobjects());
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
        Ok(Self {
            data: Arc::new(SegmentData {
                begin: begin.render_state(clock.now().to_f64()),
                animations: frozen,
                grid: open.segment(),
                base_frame: report.base_frame,
                rng_seed: rng.seed(),
            }),
        })
    }

    /// Number of nominal samples; no alpha-zero frame is invented.
    #[must_use]
    pub fn frame_count(&self) -> i64 {
        self.data.grid.n_frames()
    }

    /// Create a constant-size frame job; geometry is shared, not captured again.
    ///
    /// # Errors
    /// [`AnimError::SeekOutOfRange`] outside this play's 1-based frame range.
    pub fn job(&self, frame: i64) -> Result<PureFrameJob, AnimError> {
        let sample = self.data.grid.sample(frame).ok_or(AnimError::SeekOutOfRange {
            frame,
            total: self.frame_count(),
        })?;
        Ok(PureFrameJob { data: Arc::clone(&self.data), sample })
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
        if !open.is_at_begin()
            || animations.len() != self.data.animations.len()
            || open.report().base_frame != self.data.base_frame
            || open.report().n_frames != self.frame_count()
            || clock.now().frames() != self.data.base_frame
            || clock.fps() != self.data.grid.end_time().fps()
        {
            return Err(AnimError::InvalidFramePhase("pure play no longer at its begin boundary"));
        }
        let last = self.data.grid.end_time().to_f64();
        for animation in animations.iter_mut() {
            let alpha = last / animation.state().config.run_time;
            animation.interpolate(stage, alpha);
        }
        clock.advance_frames(self.frame_count()).map_err(AnimError::Clock)?;
        stage.set_time_from_clock(clock.now().to_f64());
        finish_open_play(stage, animations, open)
    }
}

/// One sample of a live native pure play. Safe to execute out of order on any
/// worker; both mutable animation carriers and the materialized Stage are local.
#[derive(Clone)]
pub struct PureFrameJob {
    data: Arc<SegmentData>,
    sample: FrameSample,
}

impl std::fmt::Debug for PureFrameJob {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PureFrameJob").field("time", &self.time()).finish_non_exhaustive()
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
        RngRoot::from_seed(self.data.rng_seed).substream(substream)
            .fork_frame(self.time().frames().cast_unsigned())
    }

    /// Reconstruct using Choreo's native interpolation, not whole-scene lerp
    /// between guessed endpoints. In particular, arcs, lagged families and
    /// non-monotone catalog rates keep their real intermediate states.
    #[must_use]
    pub fn materialize(&self) -> Stage {
        let mut stage = self.data.begin.clone().materialize();
        for animation in &self.data.animations {
            animation.interpolate(&mut stage, self.sample.time.to_f64());
        }
        stage.set_time_from_clock(self.time().to_f64());
        stage
    }
}

/// An opt-in consumer for closed native play programs. Ordinary consumers
/// implement only `capture`; their per-frame observations remain serial.
pub trait PlayCapture {
    /// Consume an ordinary owner-thread capture.
    fn capture(&mut self, packet: crate::FramePacket);

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
    let open = crate::open_play_with_mode(stage, clock, animations, skip)?;
    if allow_pure && !skip && capture.accepts_pure_segments()
        && let Ok(segment) = PureSegment::freeze(stage, clock, rng, animations, &open)
        && capture.capture_pure_segment(&segment)
    {
        return segment.finish_on_owner(stage, clock, animations, open);
    }
    crate::frame::drive_open_play(
        stage, clock, rng, animations, skip, boundary,
        &mut |packet| capture.capture(packet), open,
    )
}
