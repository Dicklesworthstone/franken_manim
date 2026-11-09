//! Registered updater restoration and journal suffixes under a prefix-sized budget.

use fmn_core::color::LinearRgba;
use fmn_mobject::{Mob, Stage};
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
    Checkpoint, FramePayload, JournalReplay, ServiceError, SupervisorRequest, WorkerErrorCode,
    WorkerResponse, WorkerService, protocol_digest,
};

use super::{LogEvent, RunCtx, RunOutcome, ScenarioError, fail};

const NAME: &str = "NativeRegisteredCheckpoint";

fn service_error(error: impl std::fmt::Display) -> ServiceError {
    ServiceError::new(WorkerErrorCode::ExecutionFailed, error.to_string())
}

fn drift(stage: &mut Stage, mob: Mob, dt: f64) {
    // Zero-dt prologues are observable, so crossing an empty wait must still
    // run its original lifecycle once after restoration.
    stage.shift(mob, [dt * 0.05 + 0.0002, 0.0, 0.0]);
}

fn program() -> Result<NativeSceneProgram, ServiceError> {
    let mut scene = Scene::new(RuntimeConfig::default(), 71).map_err(service_error)?;
    let root = scene
        .add_mobject(fmn_library::Circle::new().radius(0.4).build())
        .map_err(service_error)?;
    let mut registry = NativeUpdaterRegistry::new();
    registry.register_dt(
        scene.stage_mut(),
        root,
        protocol_digest(b"e2e-registered-drift-v1"),
        drift,
    )?;
    NativeSceneProgram::new(
        scene,
        vec![
            NativeSegment::WaitFrames { frames: 0 },
            NativeSegment::WaitFrames { frames: 602 },
            NativeSegment::WaitFrames { frames: 0 },
            NativeSegment::WaitFrames { frames: 298 },
        ],
        900,
    )?
    .with_checkpoint_registry(registry)
}

fn config() -> NativeWorkerConfig {
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
        protocol_digest(b"native-registered-checkpoint-e2e-build"),
        protocol_digest(include_bytes!("studio_native_resume.rs")),
        901,
        30,
        renderer,
    );
    config.replay = NativeReplayPolicy::Checkpointed;
    config
}

fn commit(
    worker: &mut NativeSceneWorker,
    frame: i64,
    journal: &mut Journal,
) -> Result<(), ScenarioError> {
    let WorkerResponse::JournalSegment {
        start_entry,
        journal: bytes,
        ..
    } = worker
        .handle(SupervisorRequest::Play {
            scene: NAME.into(),
            command: studio_seek_command(NAME, frame).map_err(|e| fail(e.to_string()))?,
        })
        .map_err(|e| fail(e.to_string()))?
    else {
        return Err(fail("registered native seek omitted its journal receipt"));
    };
    let segment = Journal::from_bytes(&bytes).map_err(|e| fail(e.to_string()))?;
    if start_entry != journal.entries().len() as u64 || segment.entries().len() != 1 {
        return Err(fail("registered native journal position changed"));
    }
    journal
        .record(
            segment.entries()[0]
                .try_clone()
                .map_err(|e| fail(e.to_string()))?,
        )
        .map_err(|e| fail(e.to_string()))?;
    Ok(())
}

fn restore(worker: &mut NativeSceneWorker, checkpoint: Checkpoint) -> Result<(), ScenarioError> {
    worker
        .handle(SupervisorRequest::RestoreCheckpoint(checkpoint))
        .map_err(|e| fail(e.to_string()))?;
    let report = worker
        .last_restore()
        .ok_or_else(|| fail("restore receipt is absent"))?;
    if report.mode != NativeRestoreMode::Checkpoint
        || report.frame != 600
        || report.fallback.is_some()
    {
        return Err(fail(
            "registered native checkpoint unexpectedly replayed its prefix",
        ));
    }
    Ok(())
}

fn png(worker: &mut NativeSceneWorker, frame: i64) -> Result<Vec<u8>, ScenarioError> {
    let WorkerResponse::Frame(frame) = worker
        .handle(SupervisorRequest::Scrub {
            scene: NAME.into(),
            frame,
        })
        .map_err(|e| fail(e.to_string()))?
    else {
        return Err(fail("registered native continuation omitted its PNG"));
    };
    match frame.payload {
        FramePayload::Pipe { bytes, .. } => Ok(bytes),
        _ => Err(fail(
            "registered native continuation used an unexpected payload",
        )),
    }
}

pub(super) fn run(ctx: &mut RunCtx) -> Result<RunOutcome, ScenarioError> {
    let mut serial = program().map_err(|e| fail(e.to_string()))?;
    for _ in 0..600 {
        if serial
            .next_frame()
            .map_err(|e| fail(e.to_string()))?
            .is_none()
        {
            return Err(fail("serial registered updater ended before checkpoint"));
        }
    }
    let mut source = NativeSceneWorker::new(config(), program).map_err(|e| fail(e.to_string()))?;
    let mut journal = Journal::new();
    commit(&mut source, 600, &mut journal)?;
    let entry = &journal.entries()[0];
    let state = entry
        .checkpoint
        .clone()
        .ok_or_else(|| fail("checkpoint is absent"))?;
    if !state.starts_with(b"FMNR") {
        return Err(fail("registered wait did not produce a resumable envelope"));
    }
    let checkpoint = Checkpoint {
        scene: NAME.into(),
        after_entry: 0,
        state_hash: entry.state_hash,
        state: state.clone(),
    };
    let mut recovery_config = config();
    // Cold reconstruction of frame600 is impossible under this budget.
    // The frame601..608 suffix must be the only evaluated historical work.
    recovery_config.max_replay_frames = 8;
    let mut recovered =
        NativeSceneWorker::new(recovery_config, program).map_err(|e| fail(e.to_string()))?;
    restore(&mut recovered, checkpoint.clone())?;
    let mut next_png = Vec::new();
    for frame in 601..=604 {
        let expected = serial
            .next_frame()
            .map_err(|e| fail(e.to_string()))?
            .ok_or_else(|| fail("serial continuation ended early"))?;
        let actual_png = png(&mut recovered, frame)?;
        if actual_png != png(&mut source, frame)? {
            return Err(fail(
                "restored native pixels differ from uninterrupted pixels",
            ));
        }
        let owner = recovered
            .program()
            .ok_or_else(|| fail("restored owner is absent"))?;
        if owner.preview().scene().time() != expected.time()
            || owner.preview().scene().play_count() != serial.preview().scene().play_count()
            || owner
                .preview()
                .stage()
                .snapshot()
                .to_bytes()
                .map_err(|e| fail(e.to_string()))?
                != expected
                    .state()
                    .to_bytes()
                    .map_err(|e| fail(e.to_string()))?
        {
            return Err(fail(
                "restored wait lost geometry, clock or its pending epilogue",
            ));
        }
        next_png = actual_png;
    }
    // Restore the committed frame again: an uncommitted preview must not be
    // substituted for the journal's verified starting state.
    restore(&mut recovered, checkpoint)?;
    commit(&mut source, 605, &mut journal)?;
    commit(&mut source, 608, &mut journal)?;
    let expected_hashes: Vec<_> = journal.entries()[1..]
        .iter()
        .map(|entry| entry.state_hash)
        .collect();
    let WorkerResponse::ReplayComplete {
        from_entry,
        state_hashes,
    } = recovered
        .handle(SupervisorRequest::ReplayJournal(JournalReplay {
            scene: NAME.into(),
            from_entry: 1,
            through_entry: 3,
            journal: journal.to_bytes().map_err(|e| fail(e.to_string()))?,
        }))
        .map_err(|e| fail(e.to_string()))?
    else {
        return Err(fail("registered native journal suffix did not finish"));
    };
    if from_entry != 1 || state_hashes != expected_hashes {
        return Err(fail(
            "registered native suffix changed canonical state hashes",
        ));
    }
    let suffix_png = png(&mut recovered, 608)?;
    if suffix_png != png(&mut source, 608)? {
        return Err(fail("journal suffix continuation changed rendered pixels"));
    }
    let decoded = fmn_codec::decode_png(&suffix_png, &fmn_codec::PngLimits::default())
        .map_err(|e| fail(e.to_string()))?;
    if (decoded.width, decoded.height) != (96, 64) {
        return Err(fail("restored native frame dimensions changed"));
    }
    ctx.event(
        LogEvent::new("e2e.studio.registered_checkpoint")
            .field("checkpoint_frame", 600_u64)
            .field("work_budget", 8_u64)
            .field("continued_frames", 4_u64)
            .field("suffix_entries", 2_u64)
            .field("restore_mode", "checkpoint")
            .field("state_and_pixels_equal", "true"),
    );
    Ok(RunOutcome::ok()
        .with_counter("studio_registered_checkpoint_restores", 2)
        .with_counter("studio_registered_checkpoint_frames", 4)
        .with_counter("studio_registered_checkpoint_suffix_entries", 2)
        .with_artifact("native_resume_checkpoint.bin", state)
        .with_artifact("native_resume_next.png", next_png)
        .with_artifact("native_resume_suffix.png", suffix_png))
}
