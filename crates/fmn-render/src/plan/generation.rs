//! Fork retained semantic state without duplicating geometry or scratch.

use super::RenderPlan;
use std::collections::{HashMap, HashSet};

impl Clone for RenderPlan {
    fn clone(&self) -> Self {
        Self {
            segments: self.segments.clone(),
            styles: self.styles.clone(),
            shapes: self.shapes.clone(),
            images: self.images.clone(),
            retained: self.retained.clone(),
            // Scratch is not part of a captured frame. In particular, the
            // reachability marks may be as wide as the entire retained scene.
            // The new compiler obtains its own scratch at its next sync.
            scratch_instances: Vec::new(),
            scratch_retained: HashMap::new(),
            scratch_seen: HashSet::new(),
            scratch_live_shapes: Vec::new(),
            scratch_live_styles: Vec::new(),
            stats: self.stats,
            epoch: self.epoch,
            geometry_key: self.geometry_key,
            plan_key: self.plan_key,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        Binning, FrameConfig, FrameJob, MonoTable, RenderPlanLimits, ScreenMap, SyncError, Tiling,
        Viewport,
    };
    use fmn_core::color::LinearRgba;
    use fmn_mobject::{Mob, Mobject, RecordBuffer, RecordSchema, Stage};

    fn scene() -> (Stage, Mob) {
        let mut buffer = RecordBuffer::new(RecordSchema::vmobject(), 3).unwrap();
        assert!(buffer.write_range(
            "point",
            0,
            &[-1.5, -1.0, 0.0, 0.0, 1.25, 0.0, 1.5, -1.0, 0.0],
        ));
        assert!(buffer.write_range("fill_rgba", 0, &[0.1, 0.3, 1.0, 0.8].repeat(3)));
        let mut stage = Stage::new();
        let mob = stage.add(Mobject::from_buffer(buffer));
        stage.add_to_scene(mob).unwrap();
        (stage, mob)
    }

    fn pixels(plan: &RenderPlan, threads: usize) -> Vec<u8> {
        let viewport = Viewport {
            width: 32,
            height: 24,
        };
        let map = ScreenMap {
            scale: 4.0,
            origin: [16.0, 12.0],
            y_up: true,
        };
        let config = FrameConfig::new(
            viewport,
            map,
            LinearRgba {
                r: 0.0,
                g: 0.0,
                b: 0.0,
                a: 1.0,
            },
        );
        let mono = MonoTable::build(plan, map).unwrap();
        let binning = Binning::build(plan, viewport, Tiling::default(), map).unwrap();
        FrameJob::new(plan, &mono, &binning, config)
            .unwrap()
            .render(threads)
            .unwrap()
            .as_bytes()
            .to_vec()
    }

    fn fresh_pixels(stage: &Stage) -> Vec<u8> {
        let mut fresh = RenderPlan::new();
        fresh.sync(stage, 0).unwrap();
        pixels(&fresh, 1)
    }

    #[test]
    fn retained_generation_clone_preserves_state_but_not_compiler_scratch() {
        let (stage, _) = scene();
        let mut plan = RenderPlan::new();
        plan.sync(&stage, 0).unwrap();
        plan.sync(&stage, 0).unwrap();
        assert!(plan.scratch_instances.capacity() > 0);
        assert!(plan.scratch_retained.capacity() > 0);
        assert!(plan.scratch_seen.capacity() > 0);
        assert!(!plan.scratch_live_shapes.is_empty());
        assert!(!plan.scratch_live_styles.is_empty());

        let mut fork = plan.clone();
        assert_eq!(fork.segments().as_ptr(), plan.segments().as_ptr());
        assert_eq!(
            fork.shapes().shapes().as_ptr(),
            plan.shapes().shapes().as_ptr()
        );
        assert_eq!(fork.styles().rows().as_ptr(), plan.styles().rows().as_ptr());
        assert_eq!(fork.shapes().instances(), plan.shapes().instances());
        assert_eq!(fork.stats(), plan.stats());
        assert_eq!(fork.epoch(), plan.epoch());
        assert_eq!(fork.identity(), plan.identity());
        assert_eq!(fork.geometry_identity(), plan.geometry_identity());
        assert_eq!(fork.scratch_instances.capacity(), 0);
        assert_eq!(fork.scratch_retained.capacity(), 0);
        assert_eq!(fork.scratch_seen.capacity(), 0);
        assert_eq!(fork.scratch_live_shapes.capacity(), 0);
        assert_eq!(fork.scratch_live_styles.capacity(), 0);
        let stats = fork.sync(&stage, 0).unwrap();
        assert_eq!(stats.shapes_compiled, 0);
        assert_eq!(stats.shapes_reused, 1);
        assert_eq!(fork.segments().as_ptr(), plan.segments().as_ptr());
        assert_eq!(pixels(&fork, 4), pixels(&plan, 1));
    }

    #[test]
    fn retained_generations_share_segments_through_motion_and_restyle() {
        let (mut stage, mob) = scene();
        let mut plan = RenderPlan::new();
        plan.sync(&stage, 0).unwrap();
        let mut generations = vec![plan.clone()];
        let mut expected = vec![fresh_pixels(&stage)];
        let segments = plan.segments().as_ptr();
        for step in 0..6 {
            let mut next = generations.last().unwrap().clone();
            stage.shift(mob, [0.125, 0.0, 0.0]);
            if step == 3 {
                assert!(stage.get_mut(mob).unwrap().buffer.write_range(
                    "fill_rgba",
                    0,
                    &[1.0, 0.1, 0.2, 0.65].repeat(3),
                ));
            }
            let stats = next.sync(&stage, 0).unwrap();
            assert_eq!(stats.shapes_compiled, 0);
            assert_eq!(next.segments().as_ptr(), segments);
            assert_eq!(
                next.shapes().shapes().as_ptr(),
                plan.shapes().shapes().as_ptr()
            );
            generations.push(next);
            expected.push(fresh_pixels(&stage));
        }
        // New geometry appends privately; every earlier captured range stays
        // valid and readable even when workers execute it last.
        assert!(
            stage
                .get_mut(mob)
                .unwrap()
                .buffer
                .write_range("point", 1, &[0.0, 2.5, 0.0])
        );
        let mut changed = generations.last().unwrap().clone();
        changed.sync(&stage, 0).unwrap();
        assert_ne!(changed.segments().as_ptr(), segments);
        assert!(changed.segments().len() > plan.segments().len());
        assert_eq!(
            &changed.segments()[..plan.segments().len()],
            plan.segments()
        );
        generations.push(changed);
        expected.push(fresh_pixels(&stage));
        assert_ne!(expected[0], *expected.last().unwrap());
        for threads in [1, 4, 16] {
            for index in (0..generations.len()).rev() {
                assert_eq!(pixels(&generations[index], threads), expected[index]);
            }
        }
    }

    #[test]
    fn retained_generation_failure_and_compaction_leave_old_frames_renderable() {
        let (mut stage, mob) = scene();
        let mut original = RenderPlan::new();
        original.sync(&stage, 0).unwrap();
        let before = pixels(&original, 1);
        let mut fork = original.clone();
        assert!(
            stage
                .get_mut(mob)
                .unwrap()
                .buffer
                .write_range("point", 1, &[0.0, 2.5, 0.0])
        );
        let refusal = fork.sync_with_limits(
            &stage,
            0,
            RenderPlanLimits {
                max_retained_shapes: 1,
                ..RenderPlanLimits::default()
            },
        );
        assert!(matches!(refusal, Err(SyncError::LimitExceeded { .. })));
        assert_eq!(fork.identity(), original.identity());
        assert_eq!(fork.segments().as_ptr(), original.segments().as_ptr());
        assert_eq!(pixels(&fork, 4), before);
        fork.sync(&stage, 0).unwrap();
        let before_compaction = fork.clone();
        let after = fresh_pixels(&stage);
        let stats = fork
            .sync_with_limits(
                &stage,
                0,
                RenderPlanLimits {
                    max_unreachable_shapes: 0,
                    max_unreachable_segments: 0,
                    max_unreachable_styles: 0,
                    ..RenderPlanLimits::default()
                },
            )
            .unwrap();
        assert!(stats.compaction.is_some());
        assert!(fork.epoch() > before_compaction.epoch());
        assert_eq!(fork.shapes().shapes().len(), 1);
        assert_eq!(before_compaction.shapes().shapes().len(), 2);
        assert_eq!(original.shapes().shapes().len(), 1);
        for threads in [1, 4, 16] {
            assert_eq!(pixels(&fork, threads), after);
            assert_eq!(pixels(&before_compaction, threads), after);
            assert_eq!(pixels(&original, threads), before);
        }
    }
}
