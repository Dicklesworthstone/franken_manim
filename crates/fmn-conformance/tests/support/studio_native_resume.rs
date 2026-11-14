//! Registered updater restoration and journal suffixes under a prefix-sized budget.

use fmn_anim::{Animation, FrameMetadata, FramePacket};
use fmn_core::color::LinearRgba;
use fmn_mobject::{Mob, Stage};
use fmn_render::{
    EngineIdentity, FrameConfig, RetainedFrameRendererConfig, ScreenMap, Tiling, Viewport,
};
use fmn_scene::{
    CaptureReason, IntegrationError, Journal, LifecycleEvent, NullSceneSink, RuntimeConfig, Scene,
    SceneSink,
};
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

#[derive(Default)]
struct PureWaitSink {
    captures: u64,
    last: Option<FrameMetadata>,
    events: Vec<LifecycleEvent>,
}

impl SceneSink for PureWaitSink {
    fn event(&mut self, event: LifecycleEvent) -> Result<(), IntegrationError> {
        self.events.push(event);
        Ok(())
    }

    fn capture_state_required(&self) -> bool {
        false
    }

    fn capture(
        &mut self,
        reason: CaptureReason,
        packet: FramePacket,
    ) -> Result<(), IntegrationError> {
        self.capture_metadata(reason, packet.metadata())
    }

    fn capture_metadata(
        &mut self,
        _reason: CaptureReason,
        metadata: FrameMetadata,
    ) -> Result<(), IntegrationError> {
        self.captures += 1;
        self.last = Some(metadata);
        Ok(())
    }
}

fn pure_wait_scene() -> Result<Scene, ScenarioError> {
    let mut scene = Scene::new(
        RuntimeConfig {
            fps: 24,
            ..RuntimeConfig::default()
        },
        71,
    )
    .map_err(|e| fail(e.to_string()))?;
    scene
        .add_mobject(fmn_library::Circle::new().radius(0.4).build())
        .map_err(|e| fail(e.to_string()))?;
    Ok(scene)
}

fn pure_wait_resume() -> Result<u64, ScenarioError> {
    const FRAMES: u64 = 4096;
    const PREFIX: u64 = 7;
    let mut source = pure_wait_scene()?;
    source
        .wait_frames(3, &mut NullSceneSink)
        .map_err(|e| fail(e.to_string()))?;
    let mut original = source
        .begin_seekable_wait_frames(FRAMES, &mut NullSceneSink)
        .map_err(|e| fail(e.to_string()))?;
    for _ in 0..PREFIX {
        source
            .prepare_stepped_wait_frame(&mut original)
            .map_err(|e| fail(e.to_string()))?;
        source
            .complete_stepped_wait_frame(&mut original, &mut NullSceneSink)
            .map_err(|e| fail(e.to_string()))?;
    }
    let cursor = source
        .checkpoint_stepped_wait(&original)
        .map_err(|e| fail(e.to_string()))?;
    let bytes = source.state_bytes().map_err(|e| fail(e.to_string()))?;
    let mut resumed = pure_wait_scene()?;
    // This exercises the public low-level lifecycle contract. Native workers
    // separately refuse opaque preflight captures for checkpoint admission.
    let preflight_calls = std::rc::Rc::new(std::cell::Cell::new(0_u64));
    let observed = std::rc::Rc::clone(&preflight_calls);
    resumed
        .set_preflight_hook(move |_, _| {
            observed.set(observed.get() + 1);
            Ok(())
        })
        .map_err(|e| fail(e.to_string()))?;
    resumed
        .restore_state_bytes(&bytes)
        .map_err(|e| fail(e.to_string()))?;
    let mut wait = resumed
        .resume_stepped_wait(cursor)
        .map_err(|e| fail(e.to_string()))?;
    if resumed.state_bytes().map_err(|e| fail(e.to_string()))? != bytes {
        return Err(fail("pure wait admission replayed part of the prefix"));
    }
    let sample = resumed
        .try_advance_pure_wait(&mut wait, FRAMES - PREFIX)
        .map_err(|e| fail(e.to_string()))?
        .ok_or_else(|| fail("restored pure wait lost its unobserved advance proof"))?;
    if sample.frame_index() != 4099
        || sample.segment_frame() != 4096
        || sample.alpha().to_bits() != 1.0_f64.to_bits()
        || !wait.frames_complete()
        || resumed.play_count() != 1
        || preflight_calls.get() != 0
    {
        return Err(fail(
            "pure continuation changed its sample, callback history or pending epilogue",
        ));
    }

    // Independent oracle: ordinary serial Scene stepping, retaining only the
    // latest capture metadata so the fast scenario needs no frame archive.
    let mut control = pure_wait_scene()?;
    control
        .wait_frames(3, &mut NullSceneSink)
        .map_err(|e| fail(e.to_string()))?;
    let mut control_wait = control
        .begin_stepped_wait_frames(FRAMES, &mut NullSceneSink)
        .map_err(|e| fail(e.to_string()))?;
    let mut samples = PureWaitSink::default();
    for _ in 0..FRAMES {
        control
            .prepare_stepped_wait_frame(&mut control_wait)
            .map_err(|e| fail(e.to_string()))?;
        control
            .complete_stepped_wait_frame(&mut control_wait, &mut samples)
            .map_err(|e| fail(e.to_string()))?;
    }
    let expected = samples
        .last
        .ok_or_else(|| fail("serial pure wait omitted its last capture"))?;
    if samples.captures != FRAMES
        || sample != expected
        || sample.rng_fork("pure-resume").state() != expected.rng_fork("pure-resume").state()
        || resumed.state_bytes().map_err(|e| fail(e.to_string()))?
            != control.state_bytes().map_err(|e| fail(e.to_string()))?
    {
        return Err(fail(
            "one restored pure jump diverged from ordinary serial samples",
        ));
    }
    let mut finish = PureWaitSink::default();
    let mut control_finish = PureWaitSink::default();
    let report = resumed
        .finish_stepped_wait(wait, &mut finish)
        .map_err(|e| fail(e.to_string()))?;
    let expected_report = control
        .finish_stepped_wait(control_wait, &mut control_finish)
        .map_err(|e| fail(e.to_string()))?;
    if report.begin_state.is_some()
        || report.base_frame != 3
        || report.n_frames != 4096
        || report.purity != expected_report.purity
        || finish.events != control_finish.events
        || finish.captures != 0
        || resumed.play_count() != 2
    {
        return Err(fail(
            "pure continuation lost its original finish or fabricated a begin snapshot",
        ));
    }
    resumed
        .wait_frames(0, &mut NullSceneSink)
        .map_err(|e| fail(e.to_string()))?;
    control
        .wait_frames(0, &mut NullSceneSink)
        .map_err(|e| fail(e.to_string()))?;
    if preflight_calls.get() != 0
        || resumed.state_bytes().map_err(|e| fail(e.to_string()))?
            != control.state_bytes().map_err(|e| fail(e.to_string()))?
    {
        return Err(fail(
            "pure continuation replayed preflight on the following segment",
        ));
    }
    Ok(FRAMES - PREFIX)
}

pub(super) fn run(ctx: &mut RunCtx) -> Result<RunOutcome, ScenarioError> {
    let pure_suffix_frames = pure_wait_resume()?;
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
            .field("pure_suffix_frames", pure_suffix_frames)
            .field("pure_suffix_jumps", 1_u64)
            .field("restore_mode", "checkpoint")
            .field("state_and_pixels_equal", "true"),
    );
    Ok(RunOutcome::ok()
        .with_counter("studio_registered_checkpoint_restores", 2)
        .with_counter("studio_registered_checkpoint_frames", 4)
        .with_counter("studio_registered_checkpoint_suffix_entries", 2)
        .with_counter("studio_restored_pure_wait_frames", pure_suffix_frames)
        .with_counter("studio_restored_pure_wait_jumps", 1)
        .with_artifact("native_resume_checkpoint.bin", state)
        .with_artifact("native_resume_next.png", next_png)
        .with_artifact("native_resume_suffix.png", suffix_png))
}

fn drawing_program(writing: bool) -> Result<NativeSceneProgram, ServiceError> {
    let mut scene = Scene::new(RuntimeConfig::default(), 71).map_err(service_error)?;
    let rectangle = fmn_library::Rectangle::new()
        .width(0.8)
        .height(0.6)
        .build()
        .map_err(service_error)?;
    let root = scene
        .add_mobject(fmn_mobject::Mobject::group(vec![
            fmn_library::Circle::new().radius(0.4).build().into(),
            rectangle.into(),
        ]))
        .map_err(service_error)?;
    let children = scene.stage().get(root).unwrap().submobjects().to_vec();
    scene.stage_mut().shift(children[0], [-0.7, 0.0, 0.0]);
    scene.stage_mut().shift(children[1], [0.7, 0.0, 0.0]);
    scene.stage_mut().set_fill(
        root,
        Some(fmn_core::constants::BLUE_C),
        Some(0.8),
        None,
        true,
    );
    let mut animation = if writing {
        fmn_anim::write(scene.stage(), root)
    } else {
        fmn_anim::DrawBorderThenFill::new(root)
            .with_stroke_color(Some([0.9, 0.2, 0.4]))
            .with_stroke_width(3.5)
    };
    if writing {
        // Checkpoint both sides of a reversal through the drawing/filling
        // boundary; a resumed carrier must preserve each member's phase.
        animation.state_mut().config.rate_func =
            fmn_anim::RateFunc::Catalog(fmn_core::rate::CatalogRate::ThereAndBack);
    }
    NativeSceneProgram::new(
        scene,
        vec![
            NativeSegment::Play {
                animations: vec![Box::new(animation)],
                overrides: fmn_scene::PlayOverrides {
                    run_time_frames: Some(16),
                    ..fmn_scene::PlayOverrides::default()
                },
            },
            NativeSegment::WaitFrames { frames: 2 },
        ],
        18,
    )
}

fn group_program(nested: bool) -> Result<NativeSceneProgram, ServiceError> {
    let mut scene = Scene::new(RuntimeConfig::default(), 73).map_err(service_error)?;
    let drawn = scene
        .add_mobject(fmn_mobject::Mobject::group(vec![
            fmn_library::Circle::new().radius(0.25).build().into(),
            fmn_library::Circle::new().radius(0.4).build().into(),
        ]))
        .map_err(service_error)?;
    let children = scene.stage().get(drawn).unwrap().submobjects().to_vec();
    scene.stage_mut().shift(children[0], [-1.0, -0.5, 0.0]);
    scene.stage_mut().shift(children[1], [-1.0, 0.5, 0.0]);
    scene.stage_mut().set_fill(
        drawn,
        Some(fmn_core::constants::BLUE_C),
        Some(0.8),
        None,
        true,
    );
    let rotating = scene
        .add_mobject(
            fmn_library::Rectangle::new()
                .width(0.8)
                .height(0.3)
                .build()
                .map_err(service_error)?,
        )
        .map_err(service_error)?;
    scene.stage_mut().shift(rotating, [0.8, 0.4, 0.0]);
    let removed = scene
        .add_mobject(fmn_library::Circle::new().radius(0.3).build())
        .map_err(service_error)?;
    scene.stage_mut().shift(removed, [0.8, -0.5, 0.0]);

    let mut drawing = fmn_anim::write(scene.stage(), drawn);
    drawing.state_mut().config.run_time_frames = Some(9);
    let rotation = fmn_anim::Rotating::new(rotating)
        .with_angle(2.4)
        .with_about_point([0.8, 0.4, 0.0])
        .with_config(fmn_anim::AnimConfig {
            run_time_frames: Some(11),
            final_alpha_value: 0.35,
            ..fmn_anim::AnimConfig::default()
        });
    let mut fade = fmn_anim::v_fade_out(removed);
    fade.state_mut().config.run_time_frames = Some(7);
    let group = if nested {
        let inner = fmn_anim::AnimationGroup::with_lag_ratio(
            scene.stage_mut(),
            vec![Box::new(drawing), Box::new(rotation)],
            0.35,
        )
        .map_err(service_error)?
        .with_run_time_frames(11);
        fmn_anim::lagged_start(scene.stage_mut(), vec![Box::new(inner), Box::new(fade)])
            .map_err(service_error)?
            .with_duration_scale(0.75)
            .with_rate_func(fmn_anim::RateFunc::Catalog(
                fmn_core::rate::CatalogRate::ThereAndBack,
            ))
    } else {
        fmn_anim::AnimationGroup::with_lag_ratio(
            scene.stage_mut(),
            vec![Box::new(drawing), Box::new(rotation), Box::new(fade)],
            0.4,
        )
        .map_err(service_error)?
    };
    NativeSceneProgram::new(
        scene,
        vec![
            NativeSegment::Play {
                animations: vec![Box::new(group)],
                overrides: fmn_scene::PlayOverrides {
                    run_time_frames: Some(16),
                    ..fmn_scene::PlayOverrides::default()
                },
            },
            NativeSegment::WaitFrames { frames: 2 },
        ],
        18,
    )
}

pub(super) fn run_drawing(ctx: &mut RunCtx) -> Result<RunOutcome, ScenarioError> {
    run_prepared_checkpoints(ctx, drawing_program, "drawing", &[4, 9, 16])
}

pub(super) fn run_groups(ctx: &mut RunCtx) -> Result<RunOutcome, ScenarioError> {
    run_prepared_checkpoints(ctx, group_program, "group", &[4, 9, 16])
}

fn schedule_program(empty_wait: bool) -> Result<NativeSceneProgram, ServiceError> {
    let mut scene = Scene::new(RuntimeConfig::default(), 79).map_err(service_error)?;
    let drawn = scene
        .add_mobject(
            fmn_library::Rectangle::new()
                .width(0.9)
                .height(0.5)
                .build()
                .map_err(service_error)?,
        )
        .map_err(service_error)?;
    scene.stage_mut().shift(drawn, [-0.7, 0.0, 0.0]);
    scene.stage_mut().set_fill(
        drawn,
        Some(fmn_core::constants::BLUE_C),
        Some(0.8),
        None,
        true,
    );
    let removed = scene
        .add_mobject(fmn_library::Circle::new().radius(0.3).build())
        .map_err(service_error)?;
    scene.stage_mut().shift(removed, [0.7, 0.0, 0.0]);
    let drawing = fmn_anim::write(scene.stage(), drawn);
    // The second play must take its begin-state after Write has completed its
    // drawing/filling lifecycle. Constructing a future group is not beginning
    // it: a restored early checkpoint must retain this distinction.
    let rotation = fmn_anim::Rotating::new(drawn)
        .with_angle(1.7)
        .with_about_point([-0.7, 0.0, 0.0])
        .with_config(fmn_anim::AnimConfig {
            run_time_frames: Some(5),
            final_alpha_value: 0.6,
            ..fmn_anim::AnimConfig::default()
        });
    let mut fade = fmn_anim::v_fade_out(removed);
    fade.state_mut().config.run_time_frames = Some(3);
    let inner = fmn_anim::AnimationGroup::with_lag_ratio(
        scene.stage_mut(),
        vec![Box::new(rotation), Box::new(fade)],
        0.4,
    )
    .map_err(service_error)?;
    let group =
        fmn_anim::lagged_start(scene.stage_mut(), vec![Box::new(inner)]).map_err(service_error)?;
    let mut segments = vec![NativeSegment::Play {
        animations: vec![Box::new(drawing)],
        overrides: fmn_scene::PlayOverrides {
            run_time_frames: Some(6),
            ..fmn_scene::PlayOverrides::default()
        },
    }];
    if empty_wait {
        // This consumes a program segment and a play count without consuming
        // a frame. It must run once when leaving the restored first play.
        segments.push(NativeSegment::WaitFrames { frames: 0 });
    }
    segments.extend([
        NativeSegment::WaitFrames { frames: 2 },
        NativeSegment::Play {
            animations: vec![Box::new(group)],
            overrides: fmn_scene::PlayOverrides {
                run_time_frames: Some(7),
                ..fmn_scene::PlayOverrides::default()
            },
        },
        NativeSegment::WaitFrames { frames: 3 },
    ]);
    NativeSceneProgram::new(scene, segments, 18)
}

pub(super) fn run_schedules(ctx: &mut RunCtx) -> Result<RunOutcome, ScenarioError> {
    // Exercise an earlier play, its pending epilogue, an intermediate wait,
    // and that wait's pending epilogue. Every continuation then crosses the
    // later nested play and verifies all remaining native state and pixels.
    run_prepared_checkpoints(ctx, schedule_program, "schedule", &[3, 6, 7, 8])
}

fn run_prepared_checkpoints(
    ctx: &mut RunCtx,
    factory: fn(bool) -> Result<NativeSceneProgram, ServiceError>,
    family: &str,
    saved_frames: &[i64],
) -> Result<RunOutcome, ScenarioError> {
    let mut restored_count = 0;
    let mut matching_frames = 0;
    let mut last_checkpoint = Vec::new();
    let mut last_png = Vec::new();
    for variant in [false, true] {
        for &saved_frame in saved_frames {
            let mut original_config = config();
            original_config.frame_count = 19;
            original_config.checkpoint_frames = 1;
            let mut original =
                NativeSceneWorker::new(original_config.clone(), move || factory(variant))
                    .map_err(|e| fail(e.to_string()))?;
            let mut journal = Journal::new();
            commit(&mut original, saved_frame, &mut journal)?;
            let entry = &journal.entries()[0];
            let state = entry
                .checkpoint
                .clone()
                .ok_or_else(|| fail("native program omitted its checkpoint"))?;
            let expected = original
                .program()
                .unwrap()
                .preview()
                .scene()
                .state_bytes()
                .map_err(|e| fail(e.to_string()))?;
            // Even the shortest saved prefix cannot be replayed under this
            // budget. Recovery must restore the actual prepared lifecycle.
            original_config.max_replay_frames = 1;
            original_config.renderer.threads = 4;
            let mut recovered = NativeSceneWorker::new(original_config, move || factory(variant))
                .map_err(|e| fail(e.to_string()))?;
            recovered
                .handle(SupervisorRequest::RestoreCheckpoint(Checkpoint {
                    scene: NAME.into(),
                    after_entry: 0,
                    state_hash: entry.state_hash,
                    state: state.clone(),
                }))
                .map_err(|e| fail(e.to_string()))?;
            let receipt = recovered
                .last_restore()
                .ok_or_else(|| fail("native program restore omitted its receipt"))?;
            if receipt.mode != NativeRestoreMode::Checkpoint
                || receipt.frame != saved_frame as u64
                || receipt.fallback.is_some()
                || recovered
                    .program()
                    .unwrap()
                    .preview()
                    .scene()
                    .state_bytes()
                    .map_err(|e| fail(e.to_string()))?
                    != expected
            {
                return Err(fail(
                    "native play checkpoint replayed or changed its prepared state",
                ));
            }
            restored_count += 1;
            for frame in saved_frame + 1..=18 {
                let actual = png(&mut recovered, frame)?;
                let expected = png(&mut original, frame)?;
                let actual_state = recovered
                    .program()
                    .unwrap()
                    .preview()
                    .scene()
                    .state_bytes()
                    .map_err(|e| fail(e.to_string()))?;
                let expected_state = original
                    .program()
                    .unwrap()
                    .preview()
                    .scene()
                    .state_bytes()
                    .map_err(|e| fail(e.to_string()))?;
                if actual != expected || actual_state != expected_state {
                    return Err(fail(
                        "native play continuation changed pixels, phase or finish state",
                    ));
                }
                matching_frames += 1;
                last_png = actual;
            }
            last_checkpoint = state;
        }
    }
    ctx.event(
        LogEvent::new(&format!("e2e.studio.{family}_checkpoint"))
            .field("checkpoint_restores", restored_count)
            .field("matching_frames", matching_frames)
            .field("threads", "1,4")
            .field("prefix_replays", 0_u64),
    );
    Ok(RunOutcome::ok()
        .with_counter(
            &format!("studio_{family}_checkpoint_restores"),
            restored_count,
        )
        .with_counter(
            &format!("studio_{family}_checkpoint_frames"),
            matching_frames,
        )
        .with_artifact(&format!("native_{family}_checkpoint.bin"), last_checkpoint)
        .with_artifact(&format!("native_{family}_continued.png"), last_png))
}
