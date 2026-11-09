//! Portable native checkpoint continuation, including a real encoded PNG.
//! Run: cargo run --locked -p fmn-studio --example native_checkpoint

use std::error::Error;

use fmn_core::color::LinearRgba;
use fmn_mobject::{Mob, Mobject, RecordBuffer, RecordSchema, Stage};
use fmn_platform::fs::{FileSystem, StdFs};
use fmn_render::{
    EngineIdentity, FrameConfig, RetainedFrameRendererConfig, ScreenMap, Tiling, Viewport,
};
use fmn_scene::{Journal, RuntimeConfig, Scene};
use fmn_studio::native::{
    NativeReplayPolicy, NativeRestoreMode, NativeSceneProgram, NativeSceneWorker, NativeSegment,
    NativeUpdaterRegistry, NativeWorkerConfig,
};
use fmn_studio::protocol::studio_seek_command;
use fmn_studio::{
    Checkpoint, FramePayload, ProtocolDigest, ServiceError, SupervisorRequest, WorkerErrorCode,
    WorkerResponse, WorkerService, protocol_digest,
};

const NAME: &str = "RegisteredWaitCheckpoint";

fn error(error: impl std::fmt::Display) -> ServiceError {
    ServiceError::new(WorkerErrorCode::ExecutionFailed, error.to_string())
}

// The receiving mobject's durable geometry is the complete mutable state.
// The small zero-dt change also makes an accidental second opener observable.
fn drift(stage: &mut Stage, mob: Mob, dt: f64) {
    stage.shift(mob, [0.05 * dt + 0.0002, 0.0, 0.0]);
}

fn program() -> Result<NativeSceneProgram, ServiceError> {
    let mut scene = Scene::new(RuntimeConfig::default(), 71).map_err(error)?;
    let points = [
        [-0.5, -0.5, 0.0],
        [0.0, -0.5, 0.0],
        [0.5, -0.5, 0.0],
        [0.5, 0.0, 0.0],
        [0.5, 0.5, 0.0],
        [0.0, 0.5, 0.0],
        [-0.5, 0.5, 0.0],
        [-0.5, 0.0, 0.0],
        [-0.5, -0.5, 0.0],
    ];
    let mut buffer = RecordBuffer::new(RecordSchema::vmobject(), points.len()).map_err(error)?;
    for (index, point) in points.iter().enumerate() {
        buffer.write(index, "point", point);
        buffer.write(index, "fill_rgba", &[0.15, 0.65, 1.0, 1.0]);
        buffer.write(index, "stroke_width", &[0.0]);
    }
    let mob = scene
        .add_mobject(Mobject::from_buffer(buffer))
        .map_err(error)?;
    let mut registry = NativeUpdaterRegistry::new();
    registry.register_dt(
        scene.stage_mut(),
        mob,
        protocol_digest(b"registered-drift-v1"),
        drift,
    )?;
    NativeSceneProgram::new(scene, vec![NativeSegment::WaitFrames { frames: 900 }], 900)?
        .with_checkpoint_registry(registry)
}

fn config(build_id: ProtocolDigest) -> NativeWorkerConfig {
    let renderer = RetainedFrameRendererConfig {
        frame: FrameConfig::new(
            Viewport {
                width: 96,
                height: 64,
            },
            ScreenMap {
                scale: 20.0,
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
        build_id,
        protocol_digest(include_bytes!("native_checkpoint.rs")),
        901,
        30,
        renderer,
    );
    config.replay = NativeReplayPolicy::Checkpointed;
    config
}

fn png(worker: &mut NativeSceneWorker, frame: i64) -> Result<Vec<u8>, Box<dyn Error>> {
    match worker.handle(SupervisorRequest::Scrub {
        scene: NAME.into(),
        frame,
    })? {
        WorkerResponse::Frame(frame) => match frame.payload {
            FramePayload::Pipe { bytes, .. } => Ok(bytes),
            _ => Err("native preview did not return an encoded PNG".into()),
        },
        _ => Err("native preview did not return a frame".into()),
    }
}

fn main() -> Result<(), Box<dyn Error>> {
    let executable = std::env::current_exe()?;
    let build_id = protocol_digest(&StdFs.read_bounded(&executable, 512 * 1024 * 1024)?);
    let mut original = NativeSceneWorker::new(config(build_id), program)?;
    let WorkerResponse::JournalSegment { journal, .. } =
        original.handle(SupervisorRequest::Play {
            scene: NAME.into(),
            command: studio_seek_command(NAME, 600)?,
        })?
    else {
        return Err("committed seek did not return a journal entry".into());
    };
    let journal = Journal::from_bytes(&journal)?;
    let entry = journal.entries().first().ok_or("journal entry is absent")?;
    let state = entry.checkpoint.clone().ok_or("checkpoint is absent")?;
    if !state.starts_with(b"FMNR") {
        return Err("registered wait was not admitted for actual restoration".into());
    }
    let checkpoint_bytes = state.len();
    let mut resumed_config = config(build_id);
    // A replay of the 600-frame prefix exceeds this budget. Restoring data
    // and evaluating only frame601 fits, so this is also an execution oracle.
    resumed_config.max_replay_frames = 1;
    let mut resumed = NativeSceneWorker::new(resumed_config, program)?;
    resumed.handle(SupervisorRequest::RestoreCheckpoint(Checkpoint {
        scene: NAME.into(),
        after_entry: 0,
        state_hash: entry.state_hash,
        state,
    }))?;
    let report = resumed.last_restore().ok_or("restore receipt is absent")?;
    if report.mode != NativeRestoreMode::Checkpoint || report.fallback.is_some() {
        return Err("checkpoint unexpectedly required cold replay".into());
    }
    let expected = png(&mut original, 601)?;
    let actual = png(&mut resumed, 601)?;
    if actual != expected {
        return Err("restored continuation differs from uninterrupted pixels".into());
    }
    println!(
        "Restored frame {} from {checkpoint_bytes} checkpoint bytes with a one-frame work \
         budget; frame601 matches uninterrupted output ({} PNG bytes, SHA-256 {}).",
        report.frame,
        actual.len(),
        protocol_digest(&actual),
    );
    Ok(())
}
