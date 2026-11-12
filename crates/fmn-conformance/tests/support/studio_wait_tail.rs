//! Completed native construction/animation restored into its original wait tail.

use std::cell::Cell;

use fmn_anim::{AnimError, AnimState, Animation};
use fmn_core::color::LinearRgba;
use fmn_mobject::{Mob, Mobject, Stage};
use fmn_render::{
    EngineIdentity, FrameConfig, RetainedFrameRendererConfig, ScreenMap, Tiling, Viewport,
};
use fmn_scene::{EffectClass, Entry, Journal, PlayOverrides, RuntimeConfig, Scene};
use fmn_studio::native::{
    NativeReplayPolicy, NativeRestoreMode, NativeSceneProgram, NativeSceneWorker, NativeSegment,
    NativeUpdaterRegistry, NativeWorkerConfig,
};
use fmn_studio::protocol::studio_seek_command;
use fmn_studio::{
    Checkpoint, FramePayload, ServiceError, SupervisorRequest, WorkerErrorCode, WorkerResponse,
    WorkerService, protocol_digest,
};

use super::{LogEvent, RunCtx, RunOutcome, ScenarioError, fail};

const NAME: &str = "CompletedNativePrefixCheckpoint";
const CHECKPOINT_FRAME: i64 = 600;
const SUFFIX_FRAMES: u64 = 5;

thread_local! {
    // Write-only witnesses; neither the scene nor its callbacks read these
    // values to decide behavior. All authored mutable state is durable data.
    static PREFIX_CALLS: Cell<(u64, u64, u64)> = const { Cell::new((0, 0, 0)) };
    static UPDATER_CALLS: Cell<u64> = const { Cell::new(0) };
}

fn service_error(error: impl std::fmt::Display) -> ServiceError {
    ServiceError::new(WorkerErrorCode::ExecutionFailed, error.to_string())
}

fn drift(stage: &mut Stage, mob: Mob, dt: f64) {
    UPDATER_CALLS.with(|calls| calls.set(calls.get() + 1));
    // A repeated zero-dt opener changes geometry and therefore both proofs.
    stage.shift(mob, [dt * 0.05 + 0.0002, 0.0, 0.0]);
}

struct CountedRotation(Box<dyn Animation>);

impl Animation for CountedRotation {
    fn state(&self) -> &AnimState {
        self.0.state()
    }

    fn state_mut(&mut self) -> &mut AnimState {
        self.0.state_mut()
    }

    fn begin(&mut self, stage: &mut Stage) -> Result<(), AnimError> {
        PREFIX_CALLS.with(|calls| {
            let (builds, begins, finishes) = calls.get();
            calls.set((builds, begins + 1, finishes));
        });
        self.0.begin(stage)
    }

    fn interpolate_submobject(&mut self, stage: &mut Stage, mobs: &[Mob], alpha: f64) {
        self.0.interpolate_submobject(stage, mobs, alpha);
    }

    fn interpolate(&mut self, stage: &mut Stage, alpha: f64) {
        self.0.interpolate(stage, alpha);
    }

    fn finish(&mut self, stage: &mut Stage) {
        self.0.finish(stage);
        PREFIX_CALLS.with(|calls| {
            let (builds, begins, finishes) = calls.get();
            calls.set((builds, begins, finishes + 1));
        });
    }

    fn abort(&mut self, stage: &mut Stage) {
        self.0.abort(stage);
    }
}

fn program() -> Result<NativeSceneProgram, ServiceError> {
    let mut scene = Scene::new(RuntimeConfig::default(), 71).map_err(service_error)?;
    let rectangle = fmn_library::Rectangle::new()
        .width(1.0)
        .height(0.5)
        .build()
        .map_err(service_error)?;
    let root = scene.add_mobject(rectangle).map_err(service_error)?;
    scene.stage_mut().set_fill(
        root,
        Some(fmn_core::constants::BLUE_C),
        Some(1.0),
        None,
        true,
    );
    let mut registry = NativeUpdaterRegistry::new();
    registry.register_dt(
        scene.stage_mut(),
        root,
        protocol_digest(b"e2e-wait-tail-drift-v1"),
        drift,
    )?;
    NativeSceneProgram::new(
        scene,
        vec![
            NativeSegment::WaitFrames { frames: 3 },
            NativeSegment::defer(move |context| {
                PREFIX_CALLS.with(|calls| {
                    let (builds, begins, finishes) = calls.get();
                    calls.set((builds + 1, begins, finishes));
                });
                let offset = (context.rng_mut().next_u64() % 13) as f64 / 32.0;
                context.stage_mut().shift(root, [offset, 0.0, 0.0]);
                let buffer = context.stage().get(root).unwrap().buffer.deep_clone();
                let later = context.stage_mut().add(Mobject::from_buffer(buffer));
                context.stage_mut().shift(later, [1.5, 0.0, 0.0]);
                context.stage_mut().add_to_scene(later)?;
                Ok(vec![
                    NativeSegment::Play {
                        animations: vec![Box::new(CountedRotation(Box::new(fmn_anim::rotate(
                            root,
                            std::f64::consts::FRAC_PI_4,
                        ))))],
                        overrides: PlayOverrides {
                            // Exact dyadic seconds: ceil(30 / 8) = 4 samples.
                            run_time: Some(0.125),
                            ..PlayOverrides::default()
                        },
                    },
                    NativeSegment::WaitFrames { frames: 0 },
                ])
            }),
            // Prefix 3 + 4 plus this 593-frame wait reaches global frame 600.
            NativeSegment::WaitFrames { frames: 593 },
            NativeSegment::Wait {
                duration: Some(0.0),
            },
            NativeSegment::Wait {
                duration: Some(0.125),
            },
            NativeSegment::WaitFrames { frames: 296 },
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
        protocol_digest(b"native-wait-tail-checkpoint-e2e-build"),
        protocol_digest(include_bytes!("studio_wait_tail.rs")),
        901,
        30,
        renderer,
    );
    config.replay = NativeReplayPolicy::Checkpointed;
    config.checkpoint_frames = 1;
    config
}

fn commit(worker: &mut NativeSceneWorker, frame: i64) -> Result<Entry, ScenarioError> {
    let position = worker.journal_position();
    let WorkerResponse::JournalSegment {
        start_entry,
        journal,
        ..
    } = worker
        .handle(SupervisorRequest::Play {
            scene: NAME.into(),
            command: studio_seek_command(NAME, frame).map_err(|e| fail(e.to_string()))?,
        })
        .map_err(|e| fail(e.to_string()))?
    else {
        return Err(fail("native wait-tail seek omitted its journal receipt"));
    };
    let journal = Journal::from_bytes(&journal).map_err(|e| fail(e.to_string()))?;
    if start_entry != position || journal.entries().len() != 1 {
        return Err(fail("native wait-tail seek changed its journal position"));
    }
    journal.entries()[0]
        .try_clone()
        .map_err(|e| fail(e.to_string()))
}

fn png(worker: &mut NativeSceneWorker, frame: i64) -> Result<Vec<u8>, ScenarioError> {
    let WorkerResponse::Frame(capture) = worker
        .handle(SupervisorRequest::Scrub {
            scene: NAME.into(),
            frame,
        })
        .map_err(|e| fail(e.to_string()))?
    else {
        return Err(fail("native wait-tail continuation omitted its frame"));
    };
    if capture.frame_index != frame as u64 {
        return Err(fail("native wait-tail capture changed its frame index"));
    }
    match capture.payload {
        FramePayload::Pipe { bytes, .. } => Ok(bytes),
        _ => Err(fail("native wait-tail capture omitted its encoded PNG")),
    }
}

#[derive(Debug, PartialEq, Eq)]
struct Observation {
    state: Vec<u8>,
    frame: i64,
    plays: u64,
    shape: [u64; 2],
    boundary: bool,
}

fn observe(program: &NativeSceneProgram) -> Result<Observation, ScenarioError> {
    let scene = program.preview().scene();
    Ok(Observation {
        state: scene
            .stage()
            .snapshot()
            .to_bytes()
            .map_err(|e| fail(e.to_string()))?,
        frame: scene.time().frames(),
        plays: scene.play_count(),
        shape: scene.stage().frame_shape().map(f64::to_bits),
        boundary: program.at_segment_boundary(),
    })
}

fn observe_worker(worker: &NativeSceneWorker) -> Result<Observation, ScenarioError> {
    observe(
        worker
            .program()
            .ok_or_else(|| fail("native wait-tail worker lost its scene owner"))?,
    )
}

pub(super) fn run(ctx: &mut RunCtx) -> Result<RunOutcome, ScenarioError> {
    // An ordinary owner executes every nominal frame, independent of worker
    // reconstruction, durable cursors and journal continuation optimizations.
    let mut serial = program().map_err(|e| fail(e.to_string()))?;
    for _ in 0..CHECKPOINT_FRAME {
        serial
            .next_frame()
            .map_err(|e| fail(e.to_string()))?
            .ok_or_else(|| fail("serial native prefix ended before frame600"))?;
    }
    let at_checkpoint = observe(&serial)?;
    if !at_checkpoint.boundary || at_checkpoint.plays != 3 {
        return Err(fail(
            "native checkpoint did not retain its pending wait epilogue",
        ));
    }

    let mut source = NativeSceneWorker::new(config(), program).map_err(|e| fail(e.to_string()))?;
    PREFIX_CALLS.with(|calls| calls.set((0, 0, 0)));
    let entry = commit(&mut source, CHECKPOINT_FRAME)?;
    let prefix_calls = PREFIX_CALLS.with(Cell::get);
    if prefix_calls != (1, 1, 1)
        || !matches!(entry.effect, EffectClass::Stateful(_))
        || observe_worker(&source)? != at_checkpoint
    {
        return Err(fail(
            "cold native build/play prefix or its effect evidence changed",
        ));
    }
    let state = entry
        .checkpoint
        .clone()
        .ok_or_else(|| fail("native wait-tail checkpoint is absent"))?;
    if state.get(..8) != Some(b"FMNR\x02\0\0\0") {
        return Err(fail(
            "completed native prefix did not emit its resumable tail record",
        ));
    }

    let mut expected = Vec::new();
    for frame in CHECKPOINT_FRAME + 1..=CHECKPOINT_FRAME + SUFFIX_FRAMES as i64 {
        serial
            .next_frame()
            .map_err(|e| fail(e.to_string()))?
            .ok_or_else(|| fail("serial native wait tail ended early"))?;
        let observation = observe(&serial)?;
        let encoded = png(&mut source, frame)?;
        if observe_worker(&source)? != observation {
            return Err(fail(
                "fresh worker differs from ordinary serial native playback",
            ));
        }
        expected.push((frame, observation, encoded));
    }
    let expected_entry = commit(&mut source, CHECKPOINT_FRAME + SUFFIX_FRAMES as i64)?;

    let mut recovery = config();
    recovery.max_replay_frames = SUFFIX_FRAMES;
    let mut recovered =
        NativeSceneWorker::new(recovery, program).map_err(|e| fail(e.to_string()))?;
    PREFIX_CALLS.with(|calls| calls.set((0, 0, 0)));
    UPDATER_CALLS.with(|calls| calls.set(0));
    recovered
        .handle(SupervisorRequest::RestoreCheckpoint(Checkpoint {
            scene: NAME.into(),
            after_entry: 0,
            state_hash: entry.state_hash,
            state: state.clone(),
        }))
        .map_err(|e| fail(e.to_string()))?;
    let report = recovered
        .last_restore()
        .ok_or_else(|| fail("native wait-tail restore omitted its receipt"))?;
    let restored_updaters = UPDATER_CALLS.with(Cell::get);
    if report.mode != NativeRestoreMode::Checkpoint
        || report.frame != CHECKPOINT_FRAME as u64
        || report.fallback.is_some()
        || PREFIX_CALLS.with(Cell::get) != (0, 0, 0)
        || restored_updaters != 0
        || observe_worker(&recovered)? != at_checkpoint
    {
        return Err(fail(
            "native wait-tail restore repeated prefix effects or changed its state",
        ));
    }

    let mut next_png = Vec::new();
    let mut continued_png = Vec::new();
    let mut matching_frames = 0;
    for (frame, observation, encoded) in expected {
        let actual = png(&mut recovered, frame)?;
        if actual != encoded || observe_worker(&recovered)? != observation {
            return Err(fail(
                "restored native wait tail differs from serial state or pixels",
            ));
        }
        if next_png.is_empty() {
            next_png = actual.clone();
        }
        continued_png = actual;
        matching_frames += 1;
    }
    let actual_entry = commit(&mut recovered, CHECKPOINT_FRAME + SUFFIX_FRAMES as i64)?;
    let (builds, begins, finishes) = PREFIX_CALLS.with(Cell::get);
    let prefix_replays = builds + begins + finishes;
    if prefix_replays != 0
        || actual_entry.state_hash != expected_entry.state_hash
        || actual_entry.checkpoint != expected_entry.checkpoint
        || actual_entry.effect != expected_entry.effect
    {
        return Err(fail(
            "continued native tail changed full scene/RNG bytes or replayed its prefix",
        ));
    }
    let decoded = fmn_codec::decode_png(&continued_png, &fmn_codec::PngLimits::default())
        .map_err(|e| fail(e.to_string()))?;
    if (decoded.width, decoded.height) != (96, 64)
        || !decoded
            .rgba
            .chunks_exact(4)
            .any(|pixel| pixel[..3] != [0, 0, 0])
    {
        return Err(fail(
            "native wait-tail PNG lost its geometry or expected dimensions",
        ));
    }

    ctx.event(
        LogEvent::new("e2e.studio.wait_tail_checkpoint")
            .field("checkpoint_frame", CHECKPOINT_FRAME as u64)
            .field("work_budget", SUFFIX_FRAMES)
            .field("prefix_builds", prefix_calls.0)
            .field("prefix_finishes", prefix_calls.2)
            .field("prefix_replays", prefix_replays)
            .field("restore_updater_calls", restored_updaters)
            .field("matching_frames", matching_frames)
            .field("restore_mode", "checkpoint")
            .field("state_and_pixels_equal", "true"),
    );
    Ok(RunOutcome::ok()
        .with_counter("studio_wait_tail_checkpoint_restores", 1)
        .with_counter("studio_wait_tail_checkpoint_frames", matching_frames)
        .with_counter("studio_wait_tail_checkpoint_prefix_replays", prefix_replays)
        .with_counter(
            "studio_wait_tail_checkpoint_restore_updaters",
            restored_updaters,
        )
        .with_counter("studio_wait_tail_checkpoint_suffix_hashes", 1)
        .with_artifact("native_wait_tail_checkpoint.bin", state)
        .with_artifact("native_wait_tail_next.png", next_png)
        .with_artifact("native_wait_tail_continued.png", continued_png))
}
