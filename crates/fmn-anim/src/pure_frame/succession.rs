//! Prepared native succession phases for the existing frame-worker pipeline.
//!
//! This is deliberately separate from Succession's generic effect signature.
//! Unknown lifecycles, nested successions, reversing outer rates, live views,
//! updaters and callback-owned rates retain serial playback. A private arena
//! prepares only checked concrete native carriers; workers receive snapshots
//! and existing frozen interpolation, never begin/finish hooks or trait objects.

use std::any::Any;

use fmn_mobject::{FrozenRenderState, Stage};

use super::{FrozenAnimation, freeze_animations};
use crate::animation::check_animation_error;
use crate::composition::CompositionTiming;
use crate::continuation::clone_for_native_preparation;
use crate::{AnimError, Animation, FramePosition, RationalTime, Succession};

fn candidate<'a>(stage: &Stage, animations: &'a [Box<dyn Animation>]) -> Option<&'a Succession> {
    let [animation] = animations else {
        return None;
    };
    let succession = (animation.as_ref() as &dyn Any).downcast_ref::<Succession>()?;
    if !succession.can_prepare_native_frames()
        || stage.updater_error().is_some()
        || stage.has_rollback_identity_history()
        || !crate::classify_wait(stage, false).is_pure()
    {
        return None;
    }
    // All dynamic types were checked before these recursive trait accessors.
    // Unrooted targets and future members matter as much as the visible roots.
    for root in stage
        .roots()
        .iter()
        .copied()
        .chain(succession.all_mobjects())
        .chain(succession.preflight_mobjects())
    {
        if !stage.contains(root) || stage.has_updaters_in_family(root) {
            return None;
        }
        for mob in stage.family(root) {
            if stage.get(mob)?.buffer.live_view_count() != 0 {
                return None;
            }
        }
    }
    Some(succession)
}

/// Whether retaining this begin snapshot can serve a closed native program.
/// This grants no generic Timeline/reconstruct_pure_frame purity.
pub(crate) fn can_freeze_succession(stage: &Stage, animations: &[Box<dyn Animation>]) -> bool {
    candidate(stage, animations).is_some()
}

struct Phase {
    begin: FrozenRenderState,
    animation: FrozenAnimation,
}

/// At most one prepared snapshot per member, independent of frame count.
pub(super) struct FrozenSuccession {
    timing: CompositionTiming,
    phases: Vec<Phase>,
}

impl FrozenSuccession {
    pub(super) fn compile(stage: &Stage, animations: &[Box<dyn Animation>]) -> Option<Self> {
        let succession = candidate(stage, animations)?;
        let timing = succession.composition_timing()?;
        // Complete the concrete-type proof for the whole tree BEFORE the
        // first private lifecycle. Cloning itself invokes no authored hook.
        let mut members = succession
            .animations()
            .iter()
            .map(|animation| clone_for_native_preparation(animation.as_ref()))
            .collect::<Option<Vec<_>>>()?;
        let mut working = stage.freeze_render_state().materialize();
        let count = members.len();
        let mut phases = Vec::with_capacity(count);
        for (index, animation) in members.iter_mut().enumerate() {
            // The first member has already begun on the real owner. Later
            // members begin against exactly the predecessor's finish state,
            // including its own final_alpha_value and family alignment.
            if index != 0 {
                animation.begin(&mut working).ok()?;
            }
            check_animation_error(&working, animation.as_ref()).ok()?;
            let mut frozen = freeze_animations(&working, std::slice::from_ref(animation)).ok()?;
            phases.push(Phase {
                begin: working.freeze_render_state(),
                animation: frozen.pop()?,
            });
            if index + 1 != count {
                animation.finish(&mut working);
                check_animation_error(&working, animation.as_ref()).ok()?;
            }
        }
        Some(Self { timing, phases })
    }

    pub(super) fn materialize(&self, time: RationalTime) -> Result<Stage, AnimError> {
        if self.timing.uses_exact_positions() {
            let position = self
                .timing
                .position_at_position(&FramePosition::from_frame(time.frames()), time.fps())?;
            let index = self
                .timing
                .member_at_position(&position)?
                .ok_or(AnimError::EmptyComposition)?;
            let phase = &self.phases[index];
            let mut stage = phase.begin.clone().materialize();
            if let Some(local) = self.timing.interior_frame_at_position(index, &position)? {
                phase
                    .animation
                    .interpolate_at_position(&mut stage, &local, time.fps())?;
            } else {
                phase.animation.interpolate_alpha(
                    &mut stage,
                    self.timing.sub_alpha_at_position(index, &position)?,
                )?;
            }
            if let Some(error) = phase.animation.rate_error() {
                return Err(error);
            }
            return Ok(stage);
        }
        let position = crate::animation::clip(
            self.timing
                .position_at_frame(time.frames() as f64, time.fps()),
            0.0,
            self.timing.max_end(),
        );
        // Same latest-started-member rule as Succession, including overlaps,
        // gaps, zero-duration members and multiple boundaries in one sample.
        let mut index = 0;
        for (member, interval) in self.timing.intervals().iter().enumerate() {
            if position >= interval.start {
                index = member;
            }
        }
        let phase = &self.phases[index];
        let interval = self.timing.intervals()[index];
        let mut stage = phase.begin.clone().materialize();
        if self.timing.is_frame_native()
            && let Some(local) = interval.interior_frame(position)
        {
            phase
                .animation
                .interpolate_at_frame(&mut stage, local, time.fps())?;
        } else {
            phase
                .animation
                .interpolate_alpha(&mut stage, interval.sub_alpha(position))?;
        }
        if let Some(error) = phase.animation.rate_error() {
            return Err(error);
        }
        Ok(stage)
    }
}
