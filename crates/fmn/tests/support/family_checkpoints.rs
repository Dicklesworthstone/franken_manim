//! Durable family restoration through the native API and certified renderer.

use fmn::core::color::Srgb;
use fmn::core::constants::{BLUE_C, LEFT, RED_C, RIGHT, WHITE, YELLOW};
use fmn::library::{Circle, Square, VStyle, v_group};
use fmn::mobject::{FamilyCheckpoint, Mob, Snapshot, Stage};
use fmn_render::engine::{FrameConfig, encode_frame};
use fmn_render::{
    EngineIdentity, RetainedFrameRenderer, RetainedFrameRendererConfig, ScreenMap, Tiling, Viewport,
};
use std::cell::Cell;
use std::rc::Rc;

fn render(stage: &Stage, threads: usize) -> Result<Vec<u8>, String> {
    let mut renderer = RetainedFrameRenderer::new(RetainedFrameRendererConfig {
        frame: FrameConfig::new(
            Viewport {
                width: 320,
                height: 180,
            },
            ScreenMap {
                scale: 60.0,
                origin: [160.0, 90.0],
                y_up: true,
            },
            Srgb::from_rgb8(0x33, 0x33, 0x33).to_linear(1.0),
        ),
        tiling: Tiling {
            macro_tile: 128,
            fine_tile: 16,
        },
        engine: EngineIdentity::certified(),
        threads,
    })
    .map_err(|error| error.to_string())?;
    renderer
        .render(stage, 0)
        .map_err(|error| error.to_string())?;
    encode_frame(renderer.frame()).map_err(|error| error.to_string())
}

/// Return complete certified frame comparisons and the restored frame.
pub fn verify() -> Result<(u64, Vec<u8>), String> {
    let mut stage = Stage::new();
    let family = stage.add(v_group([
        Circle::new()
            .radius(0.6)
            .color(BLUE_C)
            .build()
            .shifted(LEFT),
        Square::new()
            .side_length(1.0)
            .color(YELLOW)
            .build()
            .shifted(RIGHT),
    ]));
    let outside = stage.add(Circle::new().radius(0.25).color(WHITE));
    stage.shift(outside, [3.0, -1.0, 0.0]);
    stage
        .add_many_to_scene(&[family, outside])
        .map_err(|error| error.to_string())?;
    let checkpoint = stage
        .checkpoint_family(family)
        .map_err(|error| error.to_string())?;
    let bytes = checkpoint.to_bytes().map_err(|error| error.to_string())?;
    let original = render(&stage, 1)?;
    stage.shift(family, [0.5, 1.0, 0.0]);
    stage.set_fill(family, Some(RED_C), Some(0.8), None, true);
    stage.shift(outside, [-0.5, 2.0, 0.0]);
    let outside_points = stage.get_points(outside);
    let advanced = render(&stage, 1)?;
    let scene_bytes = stage.snapshot_bytes().map_err(|error| error.to_string())?;
    let mut expected_stage = stage.snapshot().materialize();
    expected_stage
        .restore_family_checkpoint(&checkpoint)
        .map_err(|error| error.to_string())?;
    let expected = render(&expected_stage, 1)?;
    if expected == advanced || expected == original {
        return Err("checkpoint fixture did not distinguish family and external edits".into());
    }

    let mut restored = Stage::new();
    let decoded =
        Snapshot::from_bytes(&scene_bytes, &restored).map_err(|error| error.to_string())?;
    restored.restore(&decoded.snapshot);
    let rebind = |mob: Mob| {
        let (index, generation) = stage
            .snapshot_handle(mob)
            .ok_or_else(|| "checkpoint source handle is missing".to_owned())?;
        restored
            .resolve_snapshot_handle(index, generation)
            .ok_or_else(|| "restored checkpoint handle is missing".to_owned())
    };
    let restored_family = rebind(family)?;
    let restored_outside = rebind(outside)?;
    let calls = Rc::new(Cell::new(0_u64));
    let observed_calls = Rc::clone(&calls);
    restored
        .add_updater(
            restored_family,
            move |_, _| observed_calls.set(observed_calls.get() + 1),
            false,
        )
        .map_err(|error| error.to_string())?;
    let roots = restored.roots().to_vec();
    let updaters = restored.updater_manifest();
    let decoded =
        FamilyCheckpoint::from_bytes(&bytes, &restored).map_err(|error| error.to_string())?;
    if decoded.to_bytes().map_err(|error| error.to_string())? != bytes {
        return Err("family checkpoint bytes changed on arena rebinding".into());
    }
    restored
        .restore_family_checkpoint(&decoded)
        .map_err(|error| error.to_string())?;
    if calls.get() != 0
        || restored.updater_manifest() != updaters
        || restored.roots() != roots
        || restored.get_points(restored_outside) != outside_points
    {
        return Err(
            "family restoration changed scene ownership, callbacks or unrelated content".into(),
        );
    }
    for threads in [1, 4, 16] {
        if render(&restored, threads)? != expected {
            return Err("restored family frame differs across render teams".into());
        }
    }
    restored.update(0.0);
    if calls.get() != 1 {
        return Err("family restoration lost its registered updater".into());
    }
    Ok((3, expected))
}
