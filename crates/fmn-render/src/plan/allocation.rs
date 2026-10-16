//! Retained dependencies outlive short-lived frame arenas without pinning cells.

use super::*;
use crate::{FrameConfig, RetainedFrameRenderer, RetainedFrameRendererConfig, ScreenMap, Tiling, Viewport};
use fmn_core::color::LinearRgba;
use fmn_mobject::{Mobject, RecordSchema};

fn scene() -> (Stage, Mob) {
    let mut buffer = RecordBuffer::new(RecordSchema::vmobject(), 3).unwrap();
    assert!(buffer.write_range("point", 0,
        &[-1.5, -1.0, 0.0, 0.0, 1.25, 0.0, 1.5, -1.0, 0.0]));
    assert!(buffer.write_range("fill_rgba", 0, &[0.1, 0.3, 1.0, 0.8].repeat(3)));
    let mut stage = Stage::new();
    let mob = stage.add(Mobject::from_buffer(buffer));
    stage.add_to_scene(mob).unwrap();
    (stage, mob)
}

fn config(threads: usize) -> RetainedFrameRendererConfig {
    RetainedFrameRendererConfig {
        frame: FrameConfig::new(
            Viewport { width: 32, height: 24 },
            ScreenMap { scale: 4.0, origin: [16.0, 12.0], y_up: true },
            LinearRgba { r: 0.0, g: 0.0, b: 0.0, a: 1.0 },
        ),
        tiling: Tiling::default(),
        engine: crate::EngineIdentity::certified(),
        threads,
    }
}

#[test]
fn retained_allocation_witnesses_preserve_in_place_writes_and_static_reuse() {
    let (mut stage, mob) = scene();
    let mut plan = RenderPlan::new();
    plan.sync(&stage, 0).unwrap();
    let fork = plan.clone();
    let storage = stage.get(mob).unwrap().buffer.storage_id();
    assert!(plan.retained[&mob].storage.matches(&stage.get(mob).unwrap().buffer));
    assert!(fork.retained[&mob].storage.matches(&stage.get(mob).unwrap().buffer));
    // Weak observations are not live views or snapshots; they must not turn an
    // ordinary recolor into a copy of all record storage.
    let buffer = &mut stage.get_mut(mob).unwrap().buffer;
    assert_eq!(buffer.live_view_count(), 0);
    assert!(buffer.write_range("fill_rgba", 0, &[1.0, 0.2, 0.1, 0.6].repeat(3)));
    assert_eq!(buffer.storage_id(), storage);
    let changed = plan.sync(&stage, 0).unwrap();
    assert_eq!(changed.shapes_reused, 1);
    assert_eq!(changed.styles_rebuilt, 1);
    let unchanged = plan.sync(&stage, 0).unwrap();
    assert_eq!(unchanged.shapes_reused, 1);
    assert_eq!(unchanged.styles_rebuilt, 0);
}

#[test]
fn retained_allocations_distinguish_dropped_sibling_frames_with_equal_revisions() {
    let (stage, mob) = scene();
    let begin = stage.freeze_render_state();
    drop(stage);
    for threads in [1, 4, 16] {
        let mut renderer = RetainedFrameRenderer::new(config(threads)).unwrap();
        let mut revision = None;
        let mut prior_storage = None;
        let mut previous_pixels: Option<Vec<u8>> = None;
        for index in 0..64 {
            // Every frame forks the SAME revision baseline, then makes the
            // SAME number of writes. The previous arena is destroyed before
            // the next one is constructed, just as on a reused render team.
            let mut frame = begin.clone().materialize();
            let height = if index % 2 == 0 { 2.25 } else { 0.25 };
            let paint = if index % 2 == 0 { [0.8, 0.1, 0.2, 0.9] } else { [0.1, 0.8, 0.2, 0.5] };
            let buffer = &mut frame.get_mut(mob).unwrap().buffer;
            assert!(buffer.write_range("point", 1, &[0.0, height, 0.0]));
            assert!(buffer.write_range("fill_rgba", 0, &paint.repeat(3)));
            let counters = (buffer.revision(), buffer.field_revision("point"), buffer.field_revision("fill_rgba"));
            if let Some(previous) = revision { assert_eq!(counters, previous); }
            revision = Some(counters);
            let storage = buffer.storage_id();
            if let Some(previous) = prior_storage {
                // This is allocator-independent: the last retained Weak keeps
                // that allocation's address unavailable without keeping cells.
                assert_ne!(storage, previous);
                assert!(!renderer.plan().retained[&mob].storage.matches(buffer));
            }
            let mut cold = RetainedFrameRenderer::new(config(1)).unwrap();
            cold.render(&frame, 0).unwrap();
            renderer.render(&frame, 0).unwrap();
            assert_eq!(renderer.frame().as_bytes(), cold.frame().as_bytes(),
                "sibling {index}, threads {threads}");
            if let Some(previous) = &previous_pixels {
                assert_ne!(renderer.frame().as_bytes(), previous.as_slice());
            }
            previous_pixels = Some(renderer.frame().as_bytes().to_vec());
            prior_storage = Some(storage);
            drop(frame);
        }
    }
}

#[test]
fn retained_allocation_guard_survives_a_refused_replacement_and_plan_fork() {
    let (mut stage, mob) = scene();
    let mut plan = RenderPlan::new();
    plan.sync(&stage, 0).unwrap();
    let original = plan.clone();
    let first_storage = stage.get(mob).unwrap().buffer.storage_id();
    let baseline = plan.identity();
    let mut replacement = RecordBuffer::new(RecordSchema::vmobject(), 3).unwrap();
    assert!(replacement.write_range("point", 0,
        &[-1.5, -1.0, 0.0, 0.0, 2.5, 0.0, 1.5, -1.0, 0.0]));
    assert!(replacement.write_range("fill_rgba", 0, &[1.0, 0.2, 0.1, 0.6].repeat(3)));
    let entry = stage.get_mut(mob).unwrap();
    assert_eq!(entry.buffer.revision(), replacement.revision());
    entry.buffer = replacement;
    assert_ne!(entry.buffer.storage_id(), first_storage);
    assert!(!original.retained[&mob].storage.matches(&entry.buffer));
    assert!(matches!(plan.sync_with_limits(&stage, 0, RenderPlanLimits {
        max_retained_shapes: 1, ..RenderPlanLimits::default()
    }), Err(SyncError::LimitExceeded { .. })));
    assert_eq!(plan.identity(), baseline);
    plan.sync(&stage, 0).unwrap();
    assert!(plan.retained[&mob].storage.matches(&stage.get(mob).unwrap().buffer));
    assert_eq!(original.identity(), baseline);
    assert_ne!(plan.identity(), baseline);
}
