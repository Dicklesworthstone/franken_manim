//! The six-step frame order + the FramePacket freeze (§9.3, §1.4, D-19,
//! fm-x79).
//!
//! The load-bearing order, exactly the Reference's
//! (`scene.py::progress_through_animations` + `update_frame`):
//!
//! 1. animation `update_mobjects(dt)` — per animation, interleaved with
//! 2. animation `interpolate(alpha)` — `alpha = t / run_time`, raw and
//!    **unclamped** (an animation shorter than its group's progression
//!    overshoots past 1; the §9.4 pipeline's clip absorbs it — that is the
//!    Reference's mechanism for unequal run_times in one `play`);
//! 3. time advances;
//! 4. **scene updaters run, observing post-interpolation state** —
//!    `always_redraw`-style scenes semantically depend on step 4 seeing
//!    step 2's output, and on time having already advanced (the
//!    Reference's `increment_time` precedes `update_mobjects`);
//! 5. capture — Rev 4's boundary: an immutable [`FramePacket`] freezes
//!    here, after which the frame no longer depends on mutable scene
//!    state;
//! 6. emit — the packet leaves through the sink, in frame order.
//!
//! This order is a contract, not an implementation choice. The update-order
//! corpus (`tests/frame_order.rs`) locks it.
//!
//! **The FramePacket** carries the frame's index and alpha, the exact
//! rational capture time, the scene's RNG identity, and a CoW freeze of
//! the scene state. It is immutable by construction: no `&mut` access
//! exists, writes to the live stage after the freeze unshare instead of
//! leaking in (O(touched) cost, inherited from the §8.1 snapshot and
//! verified by test), and RNG state is *derived*, not carried — a keyed
//! per-frame fork is a pure function of `(seed, substream name, frame
//! index)` (§6.5), so [`FramePacket::rng_fork`] reproduces any
//! substream's frame stream from two words. Camera state and the
//! revisioned references into Lumen's compiled render IR join the packet
//! when those subsystems land (fm-gw7, fm-5xm) — a subset of the final
//! abstraction, never a substitute.
//!
//! Skip-mode semantics: the whole segment advances in one step (time and
//! updaters see one big `dt`), nothing is captured or emitted — and the
//! post-play updater pass runs at `dt = 0` in **both** modes, so a skipped
//! segment leaves dt-updaters exactly where a played segment does. The
//! Reference gives that pass `dt = run_time` under skip, double-applying
//! segment time on top of its own skip step — a defect, corrected and
//! documented in BN-10.

use std::rc::Rc;

use fmn_core::rng::{Pcg64Dxsm, RngRoot};
use fmn_mobject::{Mob, Snapshot, Stage};

use crate::animation::{AnimError, Animation};
use crate::clock::{FrameSample, FrameSegment, RationalFrameClock, RationalTime};
use crate::purity::{SegmentKind, SegmentReport, classify_play, classify_wait};

/// The immutable frame boundary (§9.3 step 5): everything after capture
/// consumes only this. Cheap to hold several in flight (CoW snapshot
/// references, not deep copies — §17.4's 3–6 frame budget).
#[derive(Clone)]
pub struct FramePacket {
    frame_index: i64,
    segment_frame: i64,
    alpha: f64,
    time: RationalTime,
    rng_seed: u64,
    state: Rc<Snapshot>,
}

impl FramePacket {
    /// Freeze the current scene state. Called by the drivers at step 5;
    /// public because the Studio's scrubbing and the replay journal freeze
    /// at barriers too.
    #[must_use]
    pub fn freeze(
        stage: &Stage,
        clock: &RationalFrameClock,
        rng: &RngRoot,
        sample: &FrameSample,
    ) -> Self {
        Self {
            frame_index: clock.now().frames(),
            segment_frame: sample.frame,
            alpha: sample.alpha,
            time: clock.now(),
            rng_seed: rng.seed(),
            state: Rc::new(stage.snapshot()),
        }
    }

    /// The global frame index (the clock's frame counter at capture) —
    /// also the key every per-frame RNG fork derives from.
    #[must_use]
    pub fn frame_index(&self) -> i64 {
        self.frame_index
    }

    /// The 1-based frame number within its segment.
    #[must_use]
    pub fn segment_frame(&self) -> i64 {
        self.segment_frame
    }

    /// The segment alpha at capture (clamped to `[0, 1]`, §9.2).
    #[must_use]
    pub fn alpha(&self) -> f64 {
        self.alpha
    }

    /// The exact rational scene time at capture.
    #[must_use]
    pub fn time(&self) -> RationalTime {
        self.time
    }

    /// The frozen scene state (restore into a [`Stage`] to reconstruct the
    /// frame's front-end state — §9.5's frame-parallel workers do exactly
    /// this from the begin-state snapshot).
    #[must_use]
    pub fn state(&self) -> &Snapshot {
        &self.state
    }

    /// The keyed per-frame fork of a named substream (§6.5): a pure
    /// function of `(seed, name, frame index)`, identical from any thread
    /// in any order — the property that makes frame parallelism
    /// replay-identical by construction (D-18).
    #[must_use]
    pub fn rng_fork(&self, substream: &str) -> Pcg64Dxsm {
        RngRoot::from_seed(self.rng_seed)
            .substream(substream)
            .fork_frame(self.frame_index.cast_unsigned())
    }
}

/// Scene-updater pass without a time advance (the Reference's bare
/// `Scene.update_mobjects(dt)` — used by `finish_animations` and the top
/// of `wait`; `Stage::update` is steps 3–4 fused).
fn update_scene_mobjects(stage: &mut Stage, dt: f64) {
    for root in stage.roots().to_vec() {
        stage.update_mobject(root, dt);
    }
}

/// Whether `mob` is already in some rooted family (the Reference's
/// membership probe in `begin_animations`).
fn in_scene_family(stage: &Stage, mob: Mob) -> bool {
    stage
        .roots()
        .to_vec()
        .into_iter()
        .any(|root| stage.family(root).contains(&mob))
}

/// The Reference's `begin_animations`: begin each animation, then root any
/// animated mobject not already on stage (begin first — its starting copy
/// must not be swept into the membership probe).
fn begin_animations(
    stage: &mut Stage,
    animations: &mut [Box<dyn Animation>],
) -> Result<(), AnimError> {
    for animation in animations.iter_mut() {
        animation.begin(stage)?;
        let mobject = animation.state().mobject();
        if !in_scene_family(stage, mobject) {
            stage
                .add_to_scene(mobject)
                .map_err(|_| AnimError::StaleHandle(mobject))?;
        }
    }
    Ok(())
}

/// One sample's stepping plan: playback steps one frame at `1/fps` and
/// captures; skip steps the whole segment at once and captures nothing
/// (the Reference's `update_frame` returns before `camera.capture` when
/// skipping).
struct StepPlan {
    sample: FrameSample,
    dt: f64,
    advance: i64,
    capture: bool,
}

/// Steps 1–6 for one sample.
fn frame_step(
    stage: &mut Stage,
    clock: &mut RationalFrameClock,
    rng: &RngRoot,
    animations: &mut [Box<dyn Animation>],
    plan: &StepPlan,
    emit: &mut dyn FnMut(FramePacket),
) {
    // Steps 1–2, per animation, interleaved exactly as the Reference does.
    for animation in animations.iter_mut() {
        animation.update_mobjects(stage, plan.dt);
        let alpha = plan.sample.time.to_f64() / animation.state().config.run_time;
        animation.interpolate(stage, alpha);
    }
    // Step 3: time advances (the rational clock is the source of truth;
    // the stage's float mirror advances inside `update`, before updaters).
    clock.advance_frames(plan.advance);
    // Step 4: scene updaters, observing post-interpolation state.
    stage.update(plan.dt);
    // Steps 5–6: freeze and emit.
    if plan.capture {
        emit(FramePacket::freeze(stage, clock, rng, &plan.sample));
    }
}

/// The Reference's `finish_animations`: finish each animation, apply
/// remover cleanup, then one scene-updater pass at `dt = 0` — in both
/// modes (the skip-mode `dt = run_time` double-application is the BN-10
/// correction).
fn finish_animations(stage: &mut Stage, animations: &mut [Box<dyn Animation>]) {
    for animation in animations.iter_mut() {
        animation.finish(stage);
        // `clean_up_from_scene` is the removal decision: leaves consult
        // their own `remover` flag, composition operators (§9.4) delegate to
        // their members — a remover composed into a group is still a
        // remover, and the container never is.
        animation.clean_up_from_scene(stage);
    }
    update_scene_mobjects(stage, 0.0);
}

/// What `begin_animations` + classification establish, before any sample
/// runs: the segment on the frame grid, its classification, the frame the
/// clock stood at, and the begin-state snapshot pure segments carry.
struct Prologue {
    segment: FrameSegment,
    purity: crate::purity::Purity,
    base_frame: i64,
    begin_state: Option<Rc<Snapshot>>,
    run_time: f64,
}

/// The opening of every play segment, whole or partial.
fn play_prologue(
    stage: &mut Stage,
    clock: &RationalFrameClock,
    animations: &mut [Box<dyn Animation>],
    skip: bool,
) -> Result<Prologue, AnimError> {
    begin_animations(stage, animations)?;
    // The Reference's np.max over get_run_time (begin already widened each
    // animation's own run_time in place).
    let run_time = animations
        .iter()
        .map(|a| a.get_run_time())
        .fold(f64::NEG_INFINITY, f64::max);
    let segment = clock.segment(run_time).map_err(AnimError::Clock)?;
    let purity = classify_play(stage, animations);
    let base_frame = clock.now().frames();
    let begin_state = (purity.is_pure() && !skip).then(|| Rc::new(stage.snapshot()));
    Ok(Prologue {
        segment,
        purity,
        base_frame,
        begin_state,
        run_time,
    })
}

impl Prologue {
    /// The journal record for the segment this prologue opened.
    fn report(self, kind: SegmentKind) -> SegmentReport {
        SegmentReport {
            kind,
            purity: self.purity,
            begin_state: self.begin_state,
            base_frame: self.base_frame,
            n_frames: self.segment.n_frames(),
            run_time: self.run_time,
        }
    }
}

/// A `play()` segment **opened but not finished** — the seek/scrub handle
/// (§9.4's `Timeline`, §13.5's scrubbing).
///
/// Opening runs the whole prologue and nothing else: the animations are
/// begun, the segment is classified, and a pure segment's begin-state
/// snapshot is taken. From there the caller either reconstructs any frame in
/// O(1) through [`reconstruct_pure_frame`](crate::purity::reconstruct_pure_frame)
/// (pure segments) or steps frames with [`advance_play`] (stateful ones) —
/// in both cases through this same driver, so a sought frame and a played
/// frame are the same frame by construction rather than by agreement.
pub struct OpenSegment {
    report: SegmentReport,
    segment: FrameSegment,
    stepped: i64,
}

impl OpenSegment {
    /// The segment's journal record (classification, begin state, frame
    /// range).
    #[must_use]
    pub fn report(&self) -> &SegmentReport {
        &self.report
    }

    /// The sampling plan the segment runs on.
    #[must_use]
    pub fn segment(&self) -> FrameSegment {
        self.segment
    }

    /// How many of its frames have been stepped so far.
    #[must_use]
    pub fn stepped(&self) -> i64 {
        self.stepped
    }

    /// Consume the handle for its report (after finishing, or discarding).
    #[must_use]
    pub fn into_report(self) -> SegmentReport {
        self.report
    }
}

/// Open a `play()` segment: begin the animations, classify the segment, and
/// stop. Nothing is emitted and the clock does not move.
///
/// # Errors
/// As [`play_segment`].
pub fn open_play(
    stage: &mut Stage,
    clock: &RationalFrameClock,
    animations: &mut [Box<dyn Animation>],
) -> Result<OpenSegment, AnimError> {
    let prologue = play_prologue(stage, clock, animations, false)?;
    let segment = prologue.segment;
    Ok(OpenSegment {
        report: prologue.report(SegmentKind::Play),
        segment,
        stepped: 0,
    })
}

/// Step an open segment forward until `upto` of its frames have run,
/// emitting each. Idempotent past the end and a no-op when already there.
///
/// # Errors
/// A composition's deferred failure ([`Animation::deferred_error`]).
pub fn advance_play(
    stage: &mut Stage,
    clock: &mut RationalFrameClock,
    rng: &RngRoot,
    animations: &mut [Box<dyn Animation>],
    open: &mut OpenSegment,
    upto: i64,
    emit: &mut dyn FnMut(FramePacket),
) -> Result<(), AnimError> {
    let target = upto.clamp(open.stepped, open.segment.n_frames());
    let dt = clock.dt().to_f64();
    for sample in open
        .segment
        .samples()
        .skip(usize::try_from(open.stepped).unwrap_or(usize::MAX))
        .take(usize::try_from(target - open.stepped).unwrap_or(0))
    {
        let plan = StepPlan {
            sample,
            dt,
            advance: 1,
            capture: true,
        };
        frame_step(stage, clock, rng, animations, &plan, emit);
        open.stepped += 1;
    }
    match animations.iter().find_map(|a| a.deferred_error()) {
        Some(err) => Err(err),
        None => Ok(()),
    }
}

/// A `wait()` segment run partway and left open — the wait-side seek
/// primitive, matching [`play_segment_upto`]. No stop condition: a
/// declarative timeline has no callbacks to consult.
///
/// # Errors
/// [`AnimError::Clock`] for a non-finite or oversized duration.
pub fn wait_segment_upto(
    stage: &mut Stage,
    clock: &mut RationalFrameClock,
    rng: &RngRoot,
    duration: f64,
    upto: i64,
    emit: &mut dyn FnMut(FramePacket),
) -> Result<SegmentReport, AnimError> {
    update_scene_mobjects(stage, 0.0);
    let segment = clock.segment(duration).map_err(AnimError::Clock)?;
    let purity = classify_wait(stage, false);
    let base_frame = clock.now().frames();
    let begin_state = purity.is_pure().then(|| Rc::new(stage.snapshot()));
    let dt = clock.dt().to_f64();
    for sample in segment
        .samples()
        .take(usize::try_from(upto).unwrap_or(usize::MAX))
    {
        clock.advance_frames(1);
        stage.update(dt);
        emit(FramePacket::freeze(stage, clock, rng, &sample));
    }
    Ok(SegmentReport {
        kind: SegmentKind::Wait,
        purity,
        begin_state,
        base_frame,
        n_frames: segment.n_frames(),
        run_time: duration,
    })
}

/// One `play()` segment under the six-step frame order: begin → automatic
/// purity classification (§9.5) → the per-sample steps over the §9.2
/// progression (or the single skip step) → finish. Emits one
/// [`FramePacket`] per sample in frame order (nothing under skip) and
/// returns the segment's [`SegmentReport`] — the journal record, carrying
/// the begin-state snapshot when the segment classified pure.
///
/// The play-level `run_time`/`rate_func`/`lag_ratio` overrides and the
/// pre/post-play scaffolding (writer, window) are the scene runtime's
/// (fm-5xm) — it calls `update_rate_info` before handing animations here.
/// Frame-parallel dispatch of pure segments is fmn-runtime's (fm-3df);
/// this driver always executes serially and the report is what makes the
/// parallel path legal.
///
/// # Errors
/// [`AnimError`] from `begin` (stale handles, hollow `time_span`) or a
/// non-finite/oversized run time ([`AnimError::Clock`]).
pub fn play_segment(
    stage: &mut Stage,
    clock: &mut RationalFrameClock,
    rng: &RngRoot,
    animations: &mut [Box<dyn Animation>],
    skip: bool,
    emit: &mut dyn FnMut(FramePacket),
) -> Result<SegmentReport, AnimError> {
    let prologue = play_prologue(stage, clock, animations, skip)?;
    let segment = prologue.segment;
    let report = prologue.report(SegmentKind::Play);
    if skip {
        // The whole segment in one step: one big dt, no capture, no emit.
        if let Some(sample) = segment.skip_sample() {
            let plan = StepPlan {
                sample,
                dt: segment.end_time().to_f64(),
                advance: segment.n_frames(),
                capture: false,
            };
            frame_step(stage, clock, rng, animations, &plan, emit);
        }
    } else {
        let mut open = OpenSegment {
            report: report.clone(),
            segment,
            stepped: 0,
        };
        advance_play(
            stage,
            clock,
            rng,
            animations,
            &mut open,
            segment.n_frames(),
            emit,
        )?;
    }
    finish_animations(stage, animations);
    // A composition operator that begins members just in time has no error
    // channel inside `interpolate` (§9.4); the segment surfaces what it
    // recorded, by name, rather than leaving a frozen composition unexplained.
    if let Some(err) = animations.iter().find_map(|a| a.deferred_error()) {
        return Err(err);
    }
    Ok(report)
}

/// The Reference's `wait` / `wait_until`: an initial scene-updater pass at
/// `dt = 0` (no time advance), then per-frame steps 3–6. A stop condition
/// is checked **after** each frame's emit (the frame where it turns true
/// is emitted, then the wait ends) and forces per-frame stepping even
/// under skip (the Reference's `override_skip_animations`) — capture stays
/// off while skipping. Returns the segment's [`SegmentReport`]; a stop
/// condition demotes to stateful by vocabulary (its callback reads
/// per-frame state and decides the segment's length).
///
/// # Errors
/// [`AnimError::Clock`] for a non-finite or oversized duration.
pub fn wait_segment(
    stage: &mut Stage,
    clock: &mut RationalFrameClock,
    rng: &RngRoot,
    duration: f64,
    mut stop_condition: Option<&mut dyn FnMut(&Stage) -> bool>,
    skip: bool,
    emit: &mut dyn FnMut(FramePacket),
) -> Result<SegmentReport, AnimError> {
    update_scene_mobjects(stage, 0.0);
    let segment = clock.segment(duration).map_err(AnimError::Clock)?;
    let purity = classify_wait(stage, stop_condition.is_some());
    let base_frame = clock.now().frames();
    let begin_state = (purity.is_pure() && !skip).then(|| Rc::new(stage.snapshot()));
    let report = SegmentReport {
        kind: SegmentKind::Wait,
        purity,
        begin_state,
        base_frame,
        n_frames: segment.n_frames(),
        run_time: duration,
    };
    if skip && stop_condition.is_none() {
        if let Some(sample) = segment.skip_sample() {
            let dt = segment.end_time().to_f64();
            clock.advance_frames(sample.frame);
            stage.update(dt);
            let _ = sample; // no capture, no emit under skip
        }
        return Ok(report);
    }
    let dt = clock.dt().to_f64();
    for sample in segment.samples() {
        clock.advance_frames(1);
        stage.update(dt);
        if !skip {
            emit(FramePacket::freeze(stage, clock, rng, &sample));
        }
        if let Some(condition) = stop_condition.as_deref_mut()
            && condition(stage)
        {
            break;
        }
    }
    Ok(report)
}
