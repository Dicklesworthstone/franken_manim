//! Twenty-second native catch-up, verified checkpoint restore and continuation.

use fmn_anim::{AnimConfig, RateFunc, Transform};
use fmn_core::color::LinearRgba;
use fmn_render::{
    EngineIdentity, FrameConfig, RetainedFrameRendererConfig, ScreenMap, Tiling, Viewport,
};
use fmn_scene::{EffectClass, ImpureEffectTag, Journal, PlayOverrides, RuntimeConfig, Scene};
use fmn_studio::native::{
    NativeReplayPolicy, NativeSceneProgram, NativeSceneWorker, NativeSegment, NativeWorkerConfig,
};
use fmn_studio::protocol::studio_seek_command;
use fmn_studio::{
    Checkpoint, FramePayload, ServiceError, SupervisorRequest, WorkerErrorCode, WorkerResponse,
    WorkerService, protocol_digest,
};

use super::{LogEvent, RunCtx, RunOutcome, ScenarioError, fail};

const NAME: &str = "NativePureCheckpoint";

fn service_error(error: impl std::fmt::Display) -> ServiceError {
    ServiceError::new(WorkerErrorCode::ExecutionFailed, error.to_string())
}

fn program() -> Result<NativeSceneProgram, ServiceError> {
    program_with_updater(false)
}

fn program_with_updater(stateful: bool) -> Result<NativeSceneProgram, ServiceError> {
    let mut scene = Scene::new(
        RuntimeConfig {
            fps: 30,
            ..RuntimeConfig::default()
        },
        71,
    )
    .map_err(service_error)?;
    let root = scene
        .add_mobject(fmn_library::Circle::new().radius(0.5).build().into())
        .map_err(service_error)?;
    if stateful {
        scene
            .stage_mut()
            .add_dt_updater(
                root,
                |stage, mob, dt| {
                    stage.shift(mob, [dt * 0.01, 0.0, 0.0]);
                },
                false,
            )
            .map_err(service_error)?;
    }
    let target = scene.stage_mut().copy_family(root).map_err(service_error)?;
    scene.stage_mut().shift(target, [3.0, 1.0, 0.0]);
    let animation = Transform::new(root, target).with_config(AnimConfig {
        rate_func: RateFunc::linear(),
        suspend_mobject_updating: false,
        ..AnimConfig::default()
    });
    NativeSceneProgram::new(
        scene,
        vec![NativeSegment::Play {
            animations: vec![Box::new(animation)],
            overrides: PlayOverrides {
                run_time_frames: Some(900),
                ..PlayOverrides::default()
            },
        }],
        900,
    )
}

fn config() -> NativeWorkerConfig {
    let renderer = RetainedFrameRendererConfig {
        frame: FrameConfig::new(
            Viewport {
                width: 96,
                height: 64,
            },
            ScreenMap {
                scale: 12.0,
                origin: [32.0, 32.0],
                y_up: true,
            },
            LinearRgba {
                r: 0.0,
                g: 0.0,
                b: 0.0,
                a: 1.0,
            },
        ),
        tiling: Tiling {
            macro_tile: 64,
            fine_tile: 8,
        },
        engine: EngineIdentity::certified(),
        threads: 1,
    };
    let mut config = NativeWorkerConfig::new(
        NAME,
        protocol_digest(b"native-pure-checkpoint-build"),
        protocol_digest(b"native-pure-checkpoint-source"),
        901,
        30,
        renderer,
    );
    config.replay = NativeReplayPolicy::ColdVerified;
    config
}

pub(super) fn run(ctx: &mut RunCtx) -> Result<RunOutcome, ScenarioError> {
    let map = |error: ServiceError| fail(error.to_string());
    let mut serial = program().map_err(map)?;
    for _ in 0..600 {
        if serial.next_frame().map_err(map)?.is_none() {
            return Err(fail("serial native program ended before twenty seconds"));
        }
    }
    let checkpoint_state = serial.state_bytes().map_err(map)?;
    let mut sampled = program().map_err(map)?;
    let work = sampled.advance_to_with_work(600).map_err(map)?;
    if work.frames_advanced != 600
        || work.frames_evaluated != 1
        || sampled.state_bytes().map_err(map)? != checkpoint_state
    {
        return Err(fail(
            "native pure seek lost sample elimination or canonical state",
        ));
    }
    let mut recovered = NativeSceneWorker::new(config(), program).map_err(map)?;
    recovered
        .handle(SupervisorRequest::RestoreCheckpoint(Checkpoint {
            scene: NAME.into(),
            after_entry: 0,
            state_hash: protocol_digest(&checkpoint_state),
            state: checkpoint_state.clone(),
        }))
        .map_err(map)?;
    let mut final_png = Vec::new();
    for frame in 601..=608 {
        let expected = serial
            .next_frame()
            .map_err(map)?
            .ok_or_else(|| fail("serial continuation ended early"))?;
        let WorkerResponse::Frame(actual) = recovered
            .handle(SupervisorRequest::Scrub {
                scene: NAME.into(),
                frame,
            })
            .map_err(map)?
        else {
            return Err(fail("recovered worker omitted its next PNG frame"));
        };
        let owner = recovered
            .program()
            .ok_or_else(|| fail("recovered owner disappeared"))?;
        if owner.preview().scene().time() != expected.time()
            || owner
                .preview()
                .stage()
                .snapshot()
                .to_bytes()
                .map_err(|e| fail(e.to_string()))?
                != expected
                    .materialize_stage()
                    .snapshot()
                    .to_bytes()
                    .map_err(|e| fail(e.to_string()))?
        {
            return Err(fail(
                "recovered native continuation differs from serial execution",
            ));
        }
        let FramePayload::Pipe { bytes, .. } = actual.payload else {
            return Err(fail("recovered frame was not a canonical PNG payload"));
        };
        final_png = bytes;
    }
    let decoded = fmn_codec::decode_png(&final_png, &fmn_codec::PngLimits::default())
        .map_err(|error| fail(error.to_string()))?;
    if (decoded.width, decoded.height) != (96, 64) {
        return Err(fail("recovered PNG dimensions changed"));
    }
    let WorkerResponse::JournalSegment { journal, .. } = recovered
        .handle(SupervisorRequest::Play {
            scene: NAME.into(),
            command: studio_seek_command(NAME, 608).map_err(|error| fail(error.to_string()))?,
        })
        .map_err(map)?
    else {
        return Err(fail("pure native command omitted its journal receipt"));
    };
    let journal = Journal::from_bytes(&journal).map_err(|error| fail(error.to_string()))?;
    if journal.entries().len() != 1 || journal.entries()[0].effect != EffectClass::Pure {
        return Err(fail(
            "pure native command was not classified from its actual execution",
        ));
    }
    let mut stateful =
        NativeSceneWorker::new(config(), || program_with_updater(true)).map_err(map)?;
    let WorkerResponse::JournalSegment { journal, .. } = stateful
        .handle(SupervisorRequest::Play {
            scene: NAME.into(),
            command: studio_seek_command(NAME, 600).map_err(|error| fail(error.to_string()))?,
        })
        .map_err(map)?
    else {
        return Err(fail("stateful native command omitted its journal receipt"));
    };
    let journal = Journal::from_bytes(&journal).map_err(|error| fail(error.to_string()))?;
    if journal.entries().len() != 1
        || !matches!(&journal.entries()[0].effect, EffectClass::Stateful(effects)
            if effects.contains(&ImpureEffectTag::DtUpdater))
    {
        return Err(fail(
            "native journal omitted the executed dt updater effect",
        ));
    }
    ctx.event(
        LogEvent::new("e2e.studio.pure_checkpoint")
            .field("target_frame", 600_u64)
            .field("samples_evaluated", work.frames_evaluated)
            .field("continued_frames", 8_u64)
            .field("pure_and_stateful", "true")
            .field("state_equal", "true"),
    );
    Ok(RunOutcome::ok()
        .with_counter("studio_pure_seek_samples", work.frames_evaluated)
        .with_counter("studio_pure_checkpoint_continuations", 8)
        .with_counter("studio_effect_classes", 2)
        .with_artifact("native_checkpoint.bin", checkpoint_state)
        .with_artifact("native_checkpoint_next.png", final_png))
}
