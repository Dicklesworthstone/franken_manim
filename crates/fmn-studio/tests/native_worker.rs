//! Real native callbacks, CPU pixels, canonical protocol and recovery together.

use std::cell::Cell;
use std::io::Cursor;
use std::rc::Rc;

use fmn_core::color::LinearRgba;
use fmn_mobject::{Mob, Mobject, RecordBuffer, RecordSchema, Stage};
use fmn_render::{
    EngineIdentity, FrameConfig, RetainedFrameRendererConfig, ScreenMap, Tiling, Viewport,
};
use fmn_scene::{
    EffectClass, Entry, EventPayload, Journal, Key, Modifiers, MouseButton, RuntimeConfig, Scene,
};
use fmn_studio::native::{
    NativeReplayPolicy, NativeRestoreMode, NativeSceneProgram, NativeSceneWorker, NativeSegment,
    NativeUpdaterRegistry, NativeWorkerConfig,
};
use fmn_studio::protocol::studio_seek_command;
use fmn_studio::{
    CURRENT_VERSION, Checkpoint, DebugLayerSet, FramePayload, FrameStream, JournalReplay,
    ProtocolLimits, RequestEnvelope, ServiceError, SupervisorRequest, WorkerErrorCode,
    WorkerResponse, WorkerServeOutcome, WorkerService, protocol_digest, read_response,
    serve_worker, write_request,
};

const NAME: &str = "LiveSquare";

thread_local! {
    // Write-only execution witnesses: these counters never affect authored
    // state or callback behavior. The model's actual counter is a Stage tracker.
    static REGISTERED_CALLS: Cell<(u64, u64)> = const { Cell::new((0, 0)) };
    static TAIL_PREFIX_CALLS: Cell<(u64, u64, u64)> = const { Cell::new((0, 0, 0)) };
}

const CHECKPOINT_WAIT_SCHEDULE: [u64; 3] = [350, 450, 100];

fn registered_counter_update(stage: &mut Stage, mob: Mob, dt: f64) {
    REGISTERED_CALLS.with(|calls| {
        let (total, zero_dt) = calls.get();
        calls.set((total + 1, zero_dt + u64::from(dt == 0.0)));
    });
    let counter = stage.get(mob).unwrap().submobjects()[0];
    stage.increment_tracker_value(counter, 1.0).unwrap();
    let step = stage.tracker_value(counter).unwrap();
    let shape = stage.frame_shape();
    stage
        .set_frame_shape([shape[0] + 1.0 / 1024.0, shape[1] + 1.0 / 2048.0])
        .unwrap();
    // Later samples depend on the previously authored frame shape, which is
    // outside the durable arena document and must travel in the native cursor.
    stage.shift(mob, [step * shape[0] * 0.000_000_1, 0.0, 0.0]);
    let phase = (step as u64 % 16) as f32 / 16.0;
    let fill = [0.25 + phase * 0.5, 1.0 - phase * 0.5, 0.125, 1.0];
    let entry = stage.get_mut(mob).unwrap();
    for index in 0..entry.buffer.len() {
        entry.buffer.write(index, "fill_rgba", &fill);
    }
}

fn checkpointed_program(
    seed: u64,
    version: fmn_studio::ProtocolDigest,
    schedule: [u64; 3],
) -> Result<NativeSceneProgram, ServiceError> {
    let mut scene = Scene::new(RuntimeConfig::default(), seed).unwrap();
    let _ = scene.rng_mut().next_u64();
    let _ = scene.rng_mut().next_u64();
    let points = [
        [-0.5_f32, -0.5, 0.0],
        [0.0, -0.5, 0.0],
        [0.5, -0.5, 0.0],
        [0.5, 0.0, 0.0],
        [0.5, 0.5, 0.0],
        [0.0, 0.5, 0.0],
        [-0.5, 0.5, 0.0],
        [-0.5, 0.0, 0.0],
        [-0.5, -0.5, 0.0],
    ];
    let mut buffer = RecordBuffer::new(RecordSchema::vmobject(), points.len()).unwrap();
    for (index, point) in points.iter().enumerate() {
        buffer.write(index, "point", point);
        buffer.write(index, "fill_rgba", &[0.25, 1.0, 0.125, 1.0]);
        buffer.write(index, "stroke_width", &[0.0]);
    }
    let root = scene.stage_mut().add(Mobject::from_buffer(buffer));
    let counter = scene.stage_mut().add_value_tracker(0.0);
    scene.stage_mut().attach(root, counter).unwrap();
    scene.add(&[root]).unwrap();
    let mut registry = NativeUpdaterRegistry::new();
    registry.register_dt(scene.stage_mut(), root, version, registered_counter_update)?;
    let frame_limit = schedule.iter().sum();
    NativeSceneProgram::new(
        scene,
        schedule
            .into_iter()
            .map(|frames| NativeSegment::WaitFrames { frames })
            .collect(),
        frame_limit,
    )?
    .with_checkpoint_registry(registry)
}

fn checkpointed_config() -> NativeWorkerConfig {
    let mut config = config();
    config.frame_count = 901;
    config.replay = NativeReplayPolicy::Checkpointed;
    config.source_digest = protocol_digest(b"registered-counter-and-grid-v1");
    config
}

fn checkpointed_worker() -> NativeSceneWorker {
    NativeSceneWorker::new(checkpointed_config(), || {
        checkpointed_program(
            71,
            protocol_digest(b"registered-counter-v1"),
            CHECKPOINT_WAIT_SCHEDULE,
        )
    })
    .unwrap()
}

fn checkpoint_observation(
    worker: &NativeSceneWorker,
) -> (Vec<u8>, i64, u64, EffectClass, bool, [u64; 2]) {
    let program = worker.program().unwrap();
    let scene = program.preview().scene();
    (
        scene.stage().snapshot().to_bytes().unwrap(),
        scene.time().frames(),
        scene.play_count(),
        program.effect_class(),
        program.at_segment_boundary(),
        scene.stage().frame_shape().map(f64::to_bits),
    )
}

// Instrument the real native lifecycle. The counters are write-only witnesses;
// all model state remains in the Stage and its one snapshotted RNG stream.
struct CountedTailAnimation(Box<dyn fmn_anim::Animation>);

impl fmn_anim::Animation for CountedTailAnimation {
    fn state(&self) -> &fmn_anim::AnimState {
        self.0.state()
    }

    fn state_mut(&mut self) -> &mut fmn_anim::AnimState {
        self.0.state_mut()
    }

    fn begin(&mut self, stage: &mut Stage) -> Result<(), fmn_anim::AnimError> {
        TAIL_PREFIX_CALLS.with(|calls| {
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
        TAIL_PREFIX_CALLS.with(|calls| {
            let (builds, begins, finishes) = calls.get();
            calls.set((builds, begins, finishes + 1));
        });
    }

    fn abort(&mut self, stage: &mut Stage) {
        self.0.abort(stage);
    }
}

fn wait_tail_program(transient: u8, hold: f64) -> Result<NativeSceneProgram, ServiceError> {
    let mut scene = Scene::new(
        RuntimeConfig {
            default_wait_time: hold,
            ..RuntimeConfig::default()
        },
        71,
    )
    .unwrap();
    let mut buffer = RecordBuffer::new(RecordSchema::vmobject(), 9).unwrap();
    for (index, point) in [
        [-0.5_f32, -0.5, 0.0],
        [0.0, -0.5, 0.0],
        [0.5, -0.5, 0.0],
        [0.5, 0.0, 0.0],
        [0.5, 0.5, 0.0],
        [0.0, 0.5, 0.0],
        [-0.5, 0.5, 0.0],
        [-0.5, 0.0, 0.0],
        [-0.5, -0.5, 0.0],
    ]
    .iter()
    .enumerate()
    {
        buffer.write(index, "point", point);
        buffer.write(index, "fill_rgba", &[0.25, 1.0, 0.125, 1.0]);
        buffer.write(index, "stroke_width", &[0.0]);
    }
    let root = scene.stage_mut().add(Mobject::from_buffer(buffer));
    let counter = scene.stage_mut().add_value_tracker(0.0);
    scene.stage_mut().attach(root, counter).unwrap();
    scene.add(&[root]).unwrap();
    let mut registry = NativeUpdaterRegistry::new();
    registry.register_dt(
        scene.stage_mut(),
        root,
        protocol_digest(b"registered-tail-counter-v1"),
        registered_counter_update,
    )?;
    if transient == 5 {
        scene
            .stage_mut()
            .add_updater(root, |_, _| {}, false)
            .unwrap();
    }
    let listener_state = Rc::new(Cell::new(0.0));
    if transient == 6 {
        let captured = Rc::clone(&listener_state);
        scene
            .event_dispatcher_mut()
            .add_listener(fmn_scene::EventListener::new(
                fmn_scene::EventType::KeyPress,
                fmn_scene::EventTarget::Global,
                move |_, _, _, stage| {
                    stage.shift(root, [captured.get(), 0.0, 0.0]);
                    fmn_scene::EventPropagation::Continue
                },
            ))
            .unwrap();
    }
    NativeSceneProgram::new(
        scene,
        vec![
            // This earlier wait is not resumable: executable work still lies
            // ahead. The original tail starts only after the deferred prefix.
            NativeSegment::WaitFrames { frames: 3 },
            NativeSegment::defer(move |context| {
                TAIL_PREFIX_CALLS.with(|calls| {
                    let (builds, begins, finishes) = calls.get();
                    calls.set((builds + 1, begins, finishes));
                });
                let displacement = (context.rng_mut().next_u64() % 13) as f64 / 16.0;
                context.stage_mut().shift(root, [displacement, 0.0, 0.0]);
                let buffer = context.stage().get(root).unwrap().buffer.deep_clone();
                let later = context.stage_mut().add(Mobject::from_buffer(buffer));
                context.stage_mut().shift(later, [1.5, 0.0, 0.0]);
                context.stage_mut().add_to_scene(later)?;
                match transient {
                    1 => {
                        context
                            .spans_mut()
                            .register(later, "x".into(), 0, 1, fmn_studio::SpanKind::MathGlyph)
                            .unwrap();
                    }
                    2 => {
                        context
                            .event_dispatcher_mut()
                            .add_listener(fmn_scene::EventListener::new(
                                fmn_scene::EventType::KeyPress,
                                fmn_scene::EventTarget::Global,
                                |_, _, _, _| fmn_scene::EventPropagation::Continue,
                            ))
                            .unwrap();
                    }
                    3 => context.add_sound("cue.wav", 0.0, None, None)?,
                    4 => {
                        context
                            .stage_mut()
                            .add_updater(
                                root,
                                |stage, mob| {
                                    stage.shift(mob, [0.001, 0.0, 0.0]);
                                },
                                false,
                            )
                            .unwrap();
                    }
                    5 => context.stage_mut().clear_updaters(root, true),
                    // A completed build can alter an initial listener's
                    // captured state without touching the dispatcher itself.
                    6 => listener_state.set(3.0),
                    _ => {}
                }
                Ok(vec![
                    NativeSegment::Play {
                        animations: vec![Box::new(CountedTailAnimation(Box::new(
                            fmn_anim::rotate(root, std::f64::consts::FRAC_PI_4),
                        )))],
                        overrides: fmn_scene::PlayOverrides {
                            // BN-02 ceilings the exact f64: 0.1 is greater
                            // than 1/10, so this play emits four frames at 30fps.
                            run_time: Some(0.1),
                            ..fmn_scene::PlayOverrides::default()
                        },
                    },
                    // Generated segments count against the original cumulative
                    // budget, including waits which emit no nominal captures.
                    NativeSegment::WaitFrames { frames: 0 },
                ])
            }),
            NativeSegment::WaitFrames { frames: 0 },
            NativeSegment::Wait { duration: None },
            NativeSegment::WaitFrames { frames: 400 },
            NativeSegment::Wait {
                duration: Some(0.125),
            },
        ],
        712,
    )?
    .with_checkpoint_registry(registry)
}

fn wait_tail_config(transient: u8) -> NativeWorkerConfig {
    let mut policy = config();
    policy.frame_count = 713;
    policy.source_digest = protocol_digest(&[b'n', b'a', b't', b'i', b'v', b'e', transient]);
    policy.replay = NativeReplayPolicy::Checkpointed;
    policy
}

fn wait_tail_worker(transient: u8) -> NativeSceneWorker {
    NativeSceneWorker::new(wait_tail_config(transient), move || {
        wait_tail_program(transient, 10.01)
    })
    .unwrap()
}

#[test]
fn trailing_wait_checkpoint_skips_completed_build_and_play_and_continues_exactly() {
    let mut original = wait_tail_worker(0);
    TAIL_PREFIX_CALLS.with(|calls| calls.set((0, 0, 0)));
    let entry = commit(&mut original, 600);
    assert_eq!(TAIL_PREFIX_CALLS.with(Cell::get), (1, 1, 1));
    let saved = checkpoint(&entry, 0);
    assert_eq!(&saved.state[..8], b"FMNR\x02\0\0\0");
    let at_checkpoint = checkpoint_observation(&original);
    assert_eq!(at_checkpoint.1, 600);
    assert_eq!(at_checkpoint.2, 5);
    assert_eq!(
        original.program().unwrap().preview().stage().roots().len(),
        2
    );
    let expected_capture = seek(&mut original, 603);
    let expected_observation = checkpoint_observation(&original);
    let expected_entry = commit(&mut original, 603);

    let mut policy = wait_tail_config(0);
    policy.max_replay_frames = 3;
    let mut recovered = NativeSceneWorker::new(policy, || wait_tail_program(0, 10.01)).unwrap();
    TAIL_PREFIX_CALLS.with(|calls| calls.set((0, 0, 0)));
    REGISTERED_CALLS.with(|calls| calls.set((0, 0)));
    recovered
        .handle(SupervisorRequest::RestoreCheckpoint(saved))
        .unwrap();
    assert_eq!(TAIL_PREFIX_CALLS.with(Cell::get), (0, 0, 0));
    assert_eq!(REGISTERED_CALLS.with(Cell::get), (0, 0));
    assert_eq!(checkpoint_observation(&recovered), at_checkpoint);
    assert_eq!(
        recovered.last_restore().unwrap().mode,
        NativeRestoreMode::Checkpoint
    );
    assert_eq!(recovered.last_restore().unwrap().fallback, None);

    let actual_entry = commit(&mut recovered, 603);
    assert_eq!(actual_entry.state_hash, expected_entry.state_hash);
    assert_eq!(actual_entry.checkpoint, expected_entry.checkpoint);
    assert_eq!(TAIL_PREFIX_CALLS.with(Cell::get), (0, 0, 0));
    assert_eq!(REGISTERED_CALLS.with(Cell::get), (3, 0));
    assert_eq!(checkpoint_observation(&recovered), expected_observation);
    assert_eq!(png(&seek(&mut recovered, 603)), png(&expected_capture));
}

#[test]
fn trailing_wait_restore_preserves_zero_waits_seconds_rounding_and_pending_epilogues() {
    for frame in [8, 308, 600, 708, 709, 712] {
        let mut original = wait_tail_worker(0);
        let entry = commit(&mut original, frame);
        let at_checkpoint = checkpoint_observation(&original);
        let next = (frame + 1).min(712);
        let expected = seek(&mut original, next);
        let expected_observation = checkpoint_observation(&original);
        let mut policy = wait_tail_config(0);
        policy.max_replay_frames = 1;
        let mut recovered = NativeSceneWorker::new(policy, || wait_tail_program(0, 10.01)).unwrap();
        TAIL_PREFIX_CALLS.with(|calls| calls.set((0, 0, 0)));
        REGISTERED_CALLS.with(|calls| calls.set((0, 0)));
        recovered
            .handle(SupervisorRequest::RestoreCheckpoint(checkpoint(&entry, 0)))
            .unwrap();
        assert_eq!(
            checkpoint_observation(&recovered),
            at_checkpoint,
            "frame {frame}"
        );
        assert_eq!(
            TAIL_PREFIX_CALLS.with(Cell::get),
            (0, 0, 0),
            "frame {frame}"
        );
        assert_eq!(REGISTERED_CALLS.with(Cell::get), (0, 0), "frame {frame}");
        assert_eq!(
            png(&seek(&mut recovered, next)),
            png(&expected),
            "frame {frame}"
        );
        assert_eq!(
            checkpoint_observation(&recovered),
            expected_observation,
            "frame {frame}"
        );
        assert_eq!(
            TAIL_PREFIX_CALLS.with(Cell::get),
            (0, 0, 0),
            "frame {frame}"
        );
    }
}

#[test]
fn pending_executable_work_and_transient_prefix_bindings_keep_verified_cold_restore() {
    use fmn_studio::native::NativeCheckpointFallback;

    for (transient, frame, fallback) in [
        (0, 2, NativeCheckpointFallback::UnsupportedSchedule),
        (0, 5, NativeCheckpointFallback::UnsupportedSchedule),
        // Final nominal play capture still has its pending finish lifecycle.
        (0, 7, NativeCheckpointFallback::UnsupportedSchedule),
        (1, 8, NativeCheckpointFallback::TransientSceneState),
        (2, 8, NativeCheckpointFallback::TransientSceneState),
        (3, 8, NativeCheckpointFallback::TransientSceneState),
        (4, 8, NativeCheckpointFallback::UnregisteredUpdater),
        // Removing an opaque factory callback does not make its initial
        // factory state admissible for registered continuation restoration.
        (5, 8, NativeCheckpointFallback::UnregisteredUpdater),
        (6, 8, NativeCheckpointFallback::TransientSceneState),
    ] {
        let mut original = wait_tail_worker(transient);
        let entry = commit(&mut original, frame);
        assert!(!entry.checkpoint.as_ref().unwrap().starts_with(b"FMNR"));
        let at_checkpoint = checkpoint_observation(&original);
        let mut recovered = wait_tail_worker(transient);
        recovered
            .handle(SupervisorRequest::RestoreCheckpoint(checkpoint(&entry, 0)))
            .unwrap();
        assert_eq!(checkpoint_observation(&recovered), at_checkpoint);
        let report = recovered.last_restore().unwrap();
        assert_eq!(report.mode, NativeRestoreMode::ColdVerified);
        assert_eq!(report.fallback, Some(fallback));
        assert_eq!(
            png(&seek(&mut recovered, frame + 1)),
            png(&seek(&mut original, frame + 1))
        );
    }
}

#[test]
fn trailing_wait_checkpoint_binds_original_seconds_grid_factory_and_source() {
    let mut original = wait_tail_worker(0);
    let entry = commit(&mut original, 600);
    let saved = checkpoint(&entry, 0);
    for case in 0..3 {
        let mut policy = wait_tail_config(0);
        if case == 0 {
            policy.source_digest = protocol_digest(b"changed-native-tail-source");
        }
        let mut candidate = NativeSceneWorker::new(policy, move || {
            // Both durations emit 301 frames, but their alpha laws differ.
            // The factory identity binds their original values, not the ceil.
            let program = wait_tail_program(0, if case == 1 { 10.02 } else { 10.01 })?;
            if case == 2 {
                program.with_segment_limit(100)
            } else {
                Ok(program)
            }
        })
        .unwrap();
        let before_png = seek(&mut candidate, 2);
        let before = checkpoint_observation(&candidate);
        TAIL_PREFIX_CALLS.with(|calls| calls.set((0, 0, 0)));
        REGISTERED_CALLS.with(|calls| calls.set((0, 0)));
        let error = candidate
            .handle(SupervisorRequest::RestoreCheckpoint(saved.clone()))
            .unwrap_err();
        assert_eq!(
            error.code,
            WorkerErrorCode::CheckpointRejected,
            "case {case}"
        );
        assert_eq!(TAIL_PREFIX_CALLS.with(Cell::get), (0, 0, 0));
        assert_eq!(REGISTERED_CALLS.with(Cell::get), (0, 0));
        assert_eq!(checkpoint_observation(&candidate), before);
        assert_eq!(candidate.journal_position(), 0);
        assert_eq!(candidate.last_state_hash(), None);
        assert_eq!(candidate.last_restore(), None);
        assert_eq!(png(&seek(&mut candidate, 2)), png(&before_png));
    }
}

#[test]
fn rehashed_trailing_wait_cursors_refuse_atomically_without_executing_the_prefix() {
    use fmn_hash::{Limits, Reader, Schema, UnknownPolicy, Writer};

    let schema = Schema::new(*b"FMNR", 2, 0, 0);
    let mut original = wait_tail_worker(0);
    let entry = commit(&mut original, 600);
    let saved = checkpoint(&entry, 0);
    let mut reader =
        Reader::open(&saved.state, schema, Limits::DEFAULT, UnknownPolicy::Strict).unwrap();
    let scene = reader.get_str().unwrap();
    let position = reader.get_u64().unwrap();
    let read_count = reader.get_u32().unwrap();
    let mut reads = Vec::new();
    for _ in 0..read_count {
        reads.push((reader.get_str().unwrap(), reader.get_bytes().unwrap()));
    }
    let factory = reader.get_bytes().unwrap();
    let frame = reader.get_u64().unwrap();
    let shape = [reader.get_u64().unwrap(), reader.get_u64().unwrap()];
    let started = reader.get_u64().unwrap();
    let effects = reader.get_u8().unwrap();
    let index = reader.get_u64().unwrap();
    let prefix_frames = reader.get_i64().unwrap();
    let prefix_segments = reader.get_u64().unwrap();
    let prefix_plays = reader.get_u64().unwrap();
    assert!(reader.get_bool().unwrap());
    let base = reader.get_i64().unwrap();
    let stepped = reader.get_i64().unwrap();
    let purity = reader.get_u8().unwrap();
    let state = reader.get_bytes().unwrap();
    reader.finish().unwrap();
    assert_eq!(
        (frame, started, index, base, stepped),
        (600, 7, 2, 308, 292)
    );
    assert_eq!((prefix_frames, prefix_segments, prefix_plays), (7, 4, 3));

    let mut recovered = wait_tail_worker(0);
    recovered
        .handle(SupervisorRequest::RestoreCheckpoint(saved.clone()))
        .unwrap();
    let before = checkpoint_observation(&recovered);
    let before_png = seek(&mut recovered, 600);
    let before_report = recovered.last_restore();
    for case in [
        "tail index out of range",
        "prefix clock changed",
        "prefix clock negative",
        "prefix segment count changed",
        "prefix omitted original steps",
        "prefix exceeded segment budget",
        "prefix play count changed",
        "prefix play count exceeds steps",
        "started count changed",
        "active base changed",
        "zero stepped samples",
        "stepped beyond active wait",
        "unknown wait purity",
        "lost deferred effect",
        "missing active cursor",
        "nonfinite frame shape",
    ] {
        let mut bad_index = index;
        let mut bad_prefix_frames = prefix_frames;
        let mut bad_prefix_segments = prefix_segments;
        let mut bad_prefix_plays = prefix_plays;
        let mut bad_started = started;
        let mut bad_base = base;
        let mut bad_stepped = stepped;
        let mut bad_purity = purity;
        let mut bad_effects = effects;
        let mut bad_shape = shape;
        let mut active = true;
        match case {
            "tail index out of range" => bad_index = 4,
            "prefix clock changed" => bad_prefix_frames += 1,
            "prefix clock negative" => bad_prefix_frames = -1,
            "prefix segment count changed" => bad_prefix_segments += 1,
            "prefix omitted original steps" => bad_prefix_segments = 0,
            "prefix exceeded segment budget" => bad_prefix_segments = u64::MAX,
            "prefix play count changed" => bad_prefix_plays += 1,
            "prefix play count exceeds steps" => bad_prefix_plays = bad_prefix_segments + 1,
            "started count changed" => bad_started += 1,
            "active base changed" => bad_base += 1,
            "zero stepped samples" => bad_stepped = 0,
            "stepped beyond active wait" => bad_stepped = 401,
            "unknown wait purity" => bad_purity |= 128,
            "lost deferred effect" => bad_effects &= !4,
            "missing active cursor" => active = false,
            "nonfinite frame shape" => bad_shape[0] = f64::NAN.to_bits(),
            _ => unreachable!(),
        }
        let mut writer = Writer::new(schema);
        writer.put_str(scene).put_u64(position).put_u32(read_count);
        for (path, digest) in &reads {
            writer.put_str(path).put_bytes(digest);
        }
        writer
            .put_bytes(factory)
            .put_u64(frame)
            .put_u64(bad_shape[0])
            .put_u64(bad_shape[1])
            .put_u64(bad_started)
            .put_u8(bad_effects)
            .put_u64(bad_index)
            .put_i64(bad_prefix_frames)
            .put_u64(bad_prefix_segments)
            .put_u64(bad_prefix_plays)
            .put_bool(active);
        if active {
            writer
                .put_i64(bad_base)
                .put_i64(bad_stepped)
                .put_u8(bad_purity);
        }
        writer.put_bytes(state);
        let bad_state = writer.finish().unwrap();
        // Integrity framing is valid; the semantic cursor must still refuse.
        let state_hash = protocol_digest(&bad_state);
        TAIL_PREFIX_CALLS.with(|calls| calls.set((0, 0, 0)));
        REGISTERED_CALLS.with(|calls| calls.set((0, 0)));
        let error = recovered
            .handle(SupervisorRequest::RestoreCheckpoint(Checkpoint {
                state_hash,
                state: bad_state,
                ..saved.clone()
            }))
            .unwrap_err();
        assert_eq!(error.code, WorkerErrorCode::CheckpointRejected, "{case}");
        assert_eq!(TAIL_PREFIX_CALLS.with(Cell::get), (0, 0, 0), "{case}");
        assert_eq!(REGISTERED_CALLS.with(Cell::get), (0, 0), "{case}");
        assert_eq!(checkpoint_observation(&recovered), before, "{case}");
        assert_eq!(recovered.journal_position(), 1, "{case}");
        assert_eq!(
            recovered.last_state_hash(),
            Some(entry.state_hash),
            "{case}"
        );
        assert_eq!(recovered.last_restore(), before_report, "{case}");
        assert!(recovered.journal_tail().is_empty(), "{case}");
    }
    assert_eq!(png(&seek(&mut recovered, 600)), png(&before_png));
}

fn program() -> Result<NativeSceneProgram, ServiceError> {
    let mut scene = Scene::new(RuntimeConfig::default(), 71).unwrap();
    let points = [
        [-0.5_f32, -0.5, 0.0],
        [0.0, -0.5, 0.0],
        [0.5, -0.5, 0.0],
        [0.5, 0.0, 0.0],
        [0.5, 0.5, 0.0],
        [0.0, 0.5, 0.0],
        [-0.5, 0.5, 0.0],
        [-0.5, 0.0, 0.0],
        [-0.5, -0.5, 0.0],
    ];
    let mut buffer = RecordBuffer::new(RecordSchema::vmobject(), points.len()).unwrap();
    for (index, point) in points.iter().enumerate() {
        buffer.write(index, "point", point);
        buffer.write(index, "fill_rgba", &[1.0, 0.25, 0.0, 1.0]);
        buffer.write(index, "stroke_width", &[0.0]);
    }
    let root = scene.stage_mut().add(Mobject::from_buffer(buffer));
    scene.stage_mut().add_to_scene(root).unwrap();
    // Fresh, *mutable* closure state per factory invocation. Snapshot cloning
    // would incorrectly share this counter across replay attempts.
    let mut step = 0_u32;
    scene
        .stage_mut()
        .add_updater(
            root,
            move |stage, target| {
                step += 1;
                stage.shift_many(&[target], [f64::from(step) * 0.03, 0.0, 0.0]);
            },
            false,
        )
        .unwrap();
    NativeSceneProgram::new(
        scene,
        vec![NativeSegment::Wait {
            duration: Some(0.4),
        }],
        12,
    )
}

fn config() -> NativeWorkerConfig {
    let renderer = RetainedFrameRendererConfig {
        frame: FrameConfig::new(
            Viewport {
                width: 64,
                height: 64,
            },
            ScreenMap {
                scale: 16.0,
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
        protocol_digest(b"test-build"),
        protocol_digest(b"independent-square-code-and-inputs-v1"),
        13,
        30,
        renderer,
    );
    config.checkpoint_frames = 2;
    config.replay = NativeReplayPolicy::ColdVerified;
    config
}

fn worker() -> NativeSceneWorker {
    NativeSceneWorker::new(config(), program).unwrap()
}

fn seek(worker: &mut NativeSceneWorker, frame: i64) -> FrameStream {
    let response = worker
        .handle(SupervisorRequest::Scrub {
            scene: NAME.into(),
            frame,
        })
        .unwrap();
    let WorkerResponse::Frame(frame) = response else {
        panic!("expected pixels")
    };
    frame.validate(ProtocolLimits::default()).unwrap();
    frame
}

fn png(frame: &FrameStream) -> &[u8] {
    let FramePayload::Pipe { bytes, .. } = &frame.payload else {
        panic!("expected inline PNG")
    };
    bytes
}

fn commit(worker: &mut NativeSceneWorker, frame: i64) -> Entry {
    let response = worker
        .handle(SupervisorRequest::Play {
            scene: NAME.into(),
            command: studio_seek_command(NAME, frame).unwrap(),
        })
        .unwrap();
    let WorkerResponse::JournalSegment { journal, .. } = response else {
        panic!("expected journal")
    };
    Journal::from_bytes(&journal).unwrap().entries()[0].clone()
}

fn checkpoint(entry: &Entry, after_entry: u64) -> Checkpoint {
    Checkpoint {
        scene: NAME.into(),
        after_entry,
        state_hash: entry.state_hash,
        state: entry.checkpoint.clone().expect("checkpoint attached"),
    }
}

fn event(worker: &mut NativeSceneWorker, event: EventPayload) -> FrameStream {
    let WorkerResponse::Frame(frame) = worker
        .handle(SupervisorRequest::Event {
            scene: NAME.into(),
            event,
        })
        .unwrap()
    else {
        panic!("input must return real rendered pixels")
    };
    frame
}

#[test]
fn clean_forward_preview_continues_callbacks_and_backward_scrub_recreates_them() {
    let constructions = Rc::new(Cell::new(0));
    let observed = Rc::clone(&constructions);
    let mut worker = NativeSceneWorker::new(config(), move || {
        observed.set(observed.get() + 1);
        program()
    })
    .unwrap();
    let initial = seek(&mut worker, 0);
    let moved = seek(&mut worker, 3);
    assert_eq!(
        constructions.get(),
        1,
        "forward preview must keep the actual callback owner"
    );
    assert_ne!(png(&initial), png(&moved), "real CPU pixels must move");
    assert_eq!(moved.frame_index, 3);
    let repeated = seek(&mut worker, 0);
    assert_eq!(constructions.get(), 2);
    assert_eq!(png(&initial), png(&repeated));
    assert_eq!(png(&moved), png(&seek(&mut worker, 3)));
    assert!(!moved.render_backends.is_empty());
}

#[test]
fn live_input_undo_and_inspection_share_one_native_arena_and_scrub_resets_edits() {
    let mut worker = worker();
    let before = seek(&mut worker, 3);
    let stage = worker.program().unwrap().preview().stage();
    let center = stage.get_bounding_box(stage.roots()[0]).mid;
    event(
        &mut worker,
        EventPayload::MousePress {
            point: center,
            button: MouseButton::Left,
            modifiers: Modifiers::PRIMARY,
        },
    );
    let moved = event(
        &mut worker,
        EventPayload::KeyPress {
            key: Key::ArrowUp,
            modifiers: Modifiers::SHIFT,
        },
    );
    assert_ne!(png(&before), png(&moved));
    assert_eq!(moved.frame_index, before.frame_index);
    let inspection = worker
        .handle(SupervisorRequest::Inspect { scene: NAME.into() })
        .unwrap();
    let WorkerResponse::StudioData { bytes, digest, .. } = inspection else {
        panic!("inspection")
    };
    assert_eq!(protocol_digest(&bytes), digest);
    let json = String::from_utf8(bytes).unwrap();
    assert!(json.contains("\"input_events\":true"));
    assert!(json.contains("\"frame_index\":3"));
    let undone = event(
        &mut worker,
        EventPayload::KeyPress {
            key: Key::Character('z'),
            modifiers: Modifiers::PRIMARY,
        },
    );
    assert_eq!(png(&undone), png(&before));
    event(
        &mut worker,
        EventPayload::KeyPress {
            key: Key::ArrowUp,
            modifiers: Modifiers::SHIFT,
        },
    );
    assert_eq!(png(&seek(&mut worker, 3)), png(&before));
    assert!(worker.program().unwrap().preview().selection().is_empty());
    assert!(
        worker.journal_tail().is_empty(),
        "transient edits are not canonical commands"
    );
    assert_eq!(worker.last_state_hash(), None);
}

#[test]
fn checkpoint_cold_execution_restores_real_callback_state_for_continuation() {
    let mut original = worker();
    let entry = commit(&mut original, 3);
    let expected = seek(&mut original, 6);
    let builds = Rc::new(Cell::new(0));
    let observed = Rc::clone(&builds);
    let mut recovered = NativeSceneWorker::new(config(), move || {
        observed.set(observed.get() + 1);
        program()
    })
    .unwrap();
    let ack = recovered
        .handle(SupervisorRequest::RestoreCheckpoint(checkpoint(&entry, 0)))
        .unwrap();
    assert_eq!(
        ack,
        WorkerResponse::Ack {
            state_hash: Some(entry.state_hash),
            journal_len: 1
        }
    );
    let count = builds.get();
    assert_eq!(png(&seek(&mut recovered, 6)), png(&expected));
    assert_eq!(
        builds.get(),
        count,
        "resuming the restored cursor must not rebuild callbacks again"
    );
}

#[test]
fn registered_checkpoint_resumes_at_twenty_seconds_without_evaluating_the_prefix() {
    let mut original = checkpointed_worker();
    let entry = commit(&mut original, 600);
    let saved = checkpoint(&entry, 0);
    let at_checkpoint = checkpoint_observation(&original);
    assert_eq!(at_checkpoint.1, 600);
    assert_eq!(at_checkpoint.2, 1, "second wait is still open");
    assert!(matches!(at_checkpoint.3, EffectClass::Stateful(_)));
    let original_root = original.program().unwrap().preview().stage().roots()[0];
    let mut expected = Vec::new();
    for frame in [601, 603, 800, 801, 802] {
        let capture = seek(&mut original, frame);
        expected.push((
            frame,
            png(&capture).to_vec(),
            checkpoint_observation(&original),
        ));
    }

    let builds = Rc::new(Cell::new(0_u32));
    let observed = Rc::clone(&builds);
    let mut recovered = NativeSceneWorker::new(checkpointed_config(), move || {
        observed.set(observed.get() + 1);
        checkpointed_program(
            71,
            protocol_digest(b"registered-counter-v1"),
            CHECKPOINT_WAIT_SCHEDULE,
        )
    })
    .unwrap();
    let before_builds = builds.get();
    REGISTERED_CALLS.with(|calls| calls.set((0, 0)));
    let ack = recovered
        .handle(SupervisorRequest::RestoreCheckpoint(saved))
        .unwrap();
    assert_eq!(
        ack,
        WorkerResponse::Ack {
            state_hash: Some(entry.state_hash),
            journal_len: 1,
        }
    );
    assert_eq!(
        builds.get(),
        before_builds + 1,
        "one fresh callback registry"
    );
    assert_eq!(
        REGISTERED_CALLS.with(Cell::get),
        (0, 0),
        "restoring data must not execute 600 historical updates or a wait prologue",
    );
    assert_eq!(checkpoint_observation(&recovered), at_checkpoint);
    assert!(
        !recovered
            .program()
            .unwrap()
            .preview()
            .stage()
            .contains(original_root)
    );
    let report = recovered.last_restore().unwrap();
    assert_eq!(report.mode, NativeRestoreMode::Checkpoint);
    assert_eq!(report.frame, 600);
    assert_eq!(report.fallback, None);
    let WorkerResponse::StudioData { bytes, .. } = recovered
        .handle(SupervisorRequest::Inspect { scene: NAME.into() })
        .unwrap()
    else {
        panic!("inspection after checkpoint restoration")
    };
    assert!(
        String::from_utf8(bytes).unwrap().contains(
            "\"native_restore\":{\"mode\":\"checkpoint\",\"frame\":600,\"fallback\":null}"
        )
    );
    assert_eq!(REGISTERED_CALLS.with(Cell::get), (0, 0));

    let bound_builds = builds.get();
    for (frame, expected_png, expected_state) in expected {
        let before_calls = REGISTERED_CALLS.with(Cell::get);
        let actual = seek(&mut recovered, frame);
        assert_eq!(png(&actual), expected_png, "PNG at frame {frame}");
        assert_eq!(
            checkpoint_observation(&recovered),
            expected_state,
            "frame {frame}"
        );
        let calls = REGISTERED_CALLS.with(Cell::get);
        if frame == 601 {
            assert_eq!(calls, (1, 0), "the next sample executes exactly once");
        } else if frame == 801 {
            assert_eq!(calls, (before_calls.0 + 2, before_calls.1 + 1));
            assert_eq!(
                recovered.program().unwrap().preview().scene().play_count(),
                2
            );
        }
    }
    assert_eq!(
        builds.get(),
        bound_builds,
        "forward continuation retains its owner"
    );
}

#[test]
fn registered_checkpoint_binds_seed_callback_version_schedule_and_read_closure() {
    let mut original = checkpointed_worker();
    let saved = checkpoint(&commit(&mut original, 600), 0);
    for changed in 0..4 {
        let mut config = checkpointed_config();
        let seed = if changed == 0 { 72 } else { 71 };
        let version = protocol_digest(if changed == 1 {
            b"registered-counter-v2"
        } else {
            b"registered-counter-v1"
        });
        let schedule = if changed == 2 {
            [400, 400, 100]
        } else {
            CHECKPOINT_WAIT_SCHEDULE
        };
        if changed == 3 {
            config.source_digest = protocol_digest(b"changed-source-closure");
        }
        let mut candidate = NativeSceneWorker::new(config, move || {
            checkpointed_program(seed, version, schedule)
        })
        .unwrap();
        let before = seek(&mut candidate, 2);
        let before_state = checkpoint_observation(&candidate);
        let error = candidate
            .handle(SupervisorRequest::RestoreCheckpoint(saved.clone()))
            .unwrap_err();
        assert_eq!(
            error.code,
            WorkerErrorCode::CheckpointRejected,
            "case {changed}"
        );
        assert_eq!(
            checkpoint_observation(&candidate),
            before_state,
            "case {changed}"
        );
        assert_eq!(candidate.journal_position(), 0);
        assert_eq!(candidate.last_state_hash(), None);
        assert_eq!(candidate.last_restore(), None);
        assert_eq!(png(&seek(&mut candidate, 2)), png(&before));
    }
}

#[test]
fn rehashed_native_continuations_reject_invalid_cursors_without_replacing_the_owner() {
    use fmn_hash::{Limits, Reader, Schema, UnknownPolicy, Writer};
    use fmn_mobject::SCENE_STATE_SCHEMA;

    let schema = Schema::new(*b"FMNR", 1, 0, 0);
    let mut original = checkpointed_worker();
    let entry = commit(&mut original, 600);
    let saved = checkpoint(&entry, 0);
    let mut reader =
        Reader::open(&saved.state, schema, Limits::DEFAULT, UnknownPolicy::Strict).unwrap();
    let scene_name = reader.get_str().unwrap();
    let position = reader.get_u64().unwrap();
    let read_count = reader.get_u32().unwrap();
    let mut reads = Vec::new();
    for _ in 0..read_count {
        reads.push((reader.get_str().unwrap(), reader.get_bytes().unwrap()));
    }
    let factory = reader.get_bytes().unwrap();
    let frame = reader.get_u64().unwrap();
    let shape = [reader.get_u64().unwrap(), reader.get_u64().unwrap()];
    let started = reader.get_u64().unwrap();
    let effects = reader.get_u8().unwrap();
    assert!(reader.get_bool().unwrap());
    let base = reader.get_i64().unwrap();
    let stepped = reader.get_i64().unwrap();
    let purity = reader.get_u8().unwrap();
    let scene_state = reader.get_bytes().unwrap();
    reader.finish().unwrap();
    assert_eq!((frame, started, base, stepped), (600, 2, 350, 250));

    let mut nested = Reader::open(
        scene_state,
        SCENE_STATE_SCHEMA,
        Limits::DEFAULT,
        UnknownPolicy::Strict,
    )
    .unwrap();
    let frames_elapsed = nested.get_i64().unwrap();
    let fps = nested.get_u32().unwrap();
    let play_count = nested.get_u64().unwrap();
    let rng = [
        nested.get_u64().unwrap(),
        nested.get_u64().unwrap(),
        nested.get_u64().unwrap(),
        nested.get_u64().unwrap(),
    ];
    let snapshot = nested.get_bytes().unwrap();
    nested.finish().unwrap();

    let mut recovered = checkpointed_worker();
    recovered
        .handle(SupervisorRequest::RestoreCheckpoint(saved.clone()))
        .unwrap();
    let before = checkpoint_observation(&recovered);
    let before_png = seek(&mut recovered, 600);
    let report = recovered.last_restore();
    for case in [
        "base outside schedule",
        "zero stepped samples",
        "stepped beyond wait",
        "unknown effect",
        "unknown wait purity",
        "missing current updater effects",
        "nonfinite frame width",
        "zero frame height",
        "missing active cursor",
        "nested clock mismatch",
        "nested play count mismatch",
        "malformed nested snapshot",
    ] {
        let mut malformed_shape = shape;
        let mut malformed_effects = effects;
        let mut malformed_base = base;
        let mut malformed_stepped = stepped;
        let mut malformed_purity = purity;
        let mut active = true;
        let mut nested_frames = frames_elapsed;
        let mut nested_plays = play_count;
        let mut nested_snapshot = snapshot;
        match case {
            "base outside schedule" => malformed_base += 1,
            "zero stepped samples" => malformed_stepped = 0,
            "stepped beyond wait" => malformed_stepped = 451,
            "unknown effect" => malformed_effects |= 128,
            "unknown wait purity" => malformed_purity |= 128,
            "missing current updater effects" => {
                malformed_effects = 0;
                malformed_purity = 0;
            }
            "nonfinite frame width" => malformed_shape[0] = f64::NAN.to_bits(),
            "zero frame height" => malformed_shape[1] = 0.0_f64.to_bits(),
            "missing active cursor" => active = false,
            "nested clock mismatch" => nested_frames += 1,
            "nested play count mismatch" => nested_plays += 1,
            "malformed nested snapshot" => nested_snapshot = b"invalid arena document",
            _ => unreachable!(),
        }
        let mut nested_writer = Writer::new(SCENE_STATE_SCHEMA);
        nested_writer
            .put_i64(nested_frames)
            .put_u32(fps)
            .put_u64(nested_plays);
        for word in rng {
            nested_writer.put_u64(word);
        }
        nested_writer.put_bytes(nested_snapshot);
        let nested_bytes = nested_writer.finish().unwrap();

        let mut writer = Writer::new(schema);
        writer
            .put_str(scene_name)
            .put_u64(position)
            .put_u32(read_count);
        for (path, digest) in &reads {
            writer.put_str(path).put_bytes(digest);
        }
        writer
            .put_bytes(factory)
            .put_u64(frame)
            .put_u64(malformed_shape[0])
            .put_u64(malformed_shape[1])
            .put_u64(started)
            .put_u8(malformed_effects)
            .put_bool(active);
        if active {
            writer
                .put_i64(malformed_base)
                .put_i64(malformed_stepped)
                .put_u8(malformed_purity);
        }
        writer.put_bytes(&nested_bytes);
        let state = writer.finish().unwrap();
        // Both containers and the protocol digest are freshly valid. Refusal
        // must come from continuation/Scene validation, not a stale checksum.
        let state_hash = protocol_digest(&state);
        REGISTERED_CALLS.with(|calls| calls.set((0, 0)));
        let error = recovered
            .handle(SupervisorRequest::RestoreCheckpoint(Checkpoint {
                state,
                state_hash,
                ..saved.clone()
            }))
            .unwrap_err();
        assert_eq!(error.code, WorkerErrorCode::CheckpointRejected, "{case}");
        assert_eq!(REGISTERED_CALLS.with(Cell::get), (0, 0), "{case}");
        assert_eq!(checkpoint_observation(&recovered), before, "{case}");
        assert_eq!(recovered.journal_position(), 1, "{case}");
        assert_eq!(
            recovered.last_state_hash(),
            Some(entry.state_hash),
            "{case}"
        );
        assert_eq!(recovered.last_restore(), report, "{case}");
        assert!(recovered.journal_tail().is_empty(), "{case}");
        assert_eq!(png(&seek(&mut recovered, 600)), png(&before_png), "{case}");
        assert_eq!(REGISTERED_CALLS.with(Cell::get), (0, 0), "{case}");
    }
}

#[test]
fn pure_waits_with_empty_registry_resume_initial_and_boundary_states() {
    fn pure_program() -> Result<NativeSceneProgram, ServiceError> {
        let mut scene = Scene::new(RuntimeConfig::default(), 71).unwrap();
        let mut buffer = RecordBuffer::new(RecordSchema::vmobject(), 3).unwrap();
        for (index, point) in [[-0.5_f32, 0.0, 0.0], [0.0, 0.5, 0.0], [0.5, 0.0, 0.0]]
            .iter()
            .enumerate()
        {
            buffer.write(index, "point", point);
            buffer.write(index, "stroke_rgba", &[0.25, 1.0, 0.125, 1.0]);
            buffer.write(index, "stroke_width", &[5.0]);
        }
        let root = scene.stage_mut().add(Mobject::from_buffer(buffer));
        scene.stage_mut().shift(root, [0.25, -0.125, 0.0]);
        scene.stage_mut().set_frame_shape([12.0, 7.0]).unwrap();
        let temporary = scene.stage_mut().add_value_tracker(7.0);
        scene.stage_mut().delete(temporary).unwrap();
        assert!(!scene.stage().has_rollback_identity_history());
        scene.add(&[root]).unwrap();
        // No explicit registry is needed when no executable updater exists.
        NativeSceneProgram::new(
            scene,
            vec![
                NativeSegment::WaitFrames { frames: 3 },
                NativeSegment::Wait {
                    duration: Some(2.0 / 30.0),
                },
            ],
            5,
        )
    }

    for frame in [0, 3, 4] {
        let mut policy = checkpointed_config();
        policy.frame_count = 6;
        let mut original = NativeSceneWorker::new(policy.clone(), pure_program).unwrap();
        let entry = commit(&mut original, frame);
        let saved = checkpoint(&entry, 0);
        assert!(saved.state.starts_with(b"FMNR"));
        let at_checkpoint = checkpoint_observation(&original);
        let expected = seek(&mut original, frame + 1);
        let expected_state = checkpoint_observation(&original);
        // Even a boundary after several frames must restore without charging
        // the old prefix against this one-frame continuation work budget.
        policy.max_replay_frames = 1;
        let mut recovered = NativeSceneWorker::new(policy, pure_program).unwrap();
        recovered
            .handle(SupervisorRequest::RestoreCheckpoint(saved))
            .unwrap();
        assert_eq!(checkpoint_observation(&recovered), at_checkpoint);
        let report = recovered.last_restore().unwrap();
        assert_eq!(report.mode, NativeRestoreMode::Checkpoint);
        assert_eq!(report.frame, frame as u64);
        assert_eq!(report.fallback, None);
        assert_eq!(png(&seek(&mut recovered, frame + 1)), png(&expected));
        assert_eq!(checkpoint_observation(&recovered), expected_state);
    }
}

#[test]
fn checkpoint_policy_reports_cold_fallback_for_opaque_mutable_callback_state() {
    let mut config = config();
    config.replay = NativeReplayPolicy::Checkpointed;
    let mut original = NativeSceneWorker::new(config.clone(), program).unwrap();
    let entry = commit(&mut original, 3);
    let expected = seek(&mut original, 6);
    let mut recovered = NativeSceneWorker::new(config, program).unwrap();
    recovered
        .handle(SupervisorRequest::RestoreCheckpoint(checkpoint(&entry, 0)))
        .unwrap();
    let report = recovered.last_restore().unwrap();
    assert_eq!(report.mode, NativeRestoreMode::ColdVerified);
    assert_eq!(report.frame, 3);
    assert!(report.fallback.is_some());
    let WorkerResponse::StudioData { bytes, .. } = recovered
        .handle(SupervisorRequest::Inspect { scene: NAME.into() })
        .unwrap()
    else {
        panic!("inspection after cold fallback")
    };
    assert!(String::from_utf8(bytes).unwrap().contains(
        "\"native_restore\":{\"mode\":\"cold_verified\",\"frame\":3,\"fallback\":\"unregistered_updater\"}"
    ));
    assert_eq!(png(&seek(&mut recovered, 6)), png(&expected));
    assert_eq!(
        checkpoint_observation(&recovered),
        checkpoint_observation(&original)
    );
}

#[test]
fn registered_suffix_charges_only_new_frames_and_commits_transactionally() {
    let mut original = checkpointed_worker();
    let first = commit(&mut original, 600);
    let saved = checkpoint(&first, 0);
    let at_checkpoint = checkpoint_observation(&original);
    let checkpoint_png = seek(&mut original, 600);
    let second = commit(&mut original, 602);
    let third = commit(&mut original, 604);
    let expected = seek(&mut original, 604);
    let expected_state = checkpoint_observation(&original);
    let mut journal = Journal::new();
    for entry in [&first, &second, &third] {
        journal.record(entry.clone()).unwrap();
    }
    let journal_bytes = journal.to_bytes().unwrap();
    let mut divergent = Journal::new();
    divergent.record(first.clone()).unwrap();
    divergent.record(second.clone()).unwrap();
    let mut wrong = third.clone();
    wrong.state_hash = protocol_digest(b"wrong final suffix state");
    wrong.checkpoint = None;
    divergent.record(wrong).unwrap();

    for budget in [3, 4] {
        let mut policy = checkpointed_config();
        policy.max_replay_frames = budget;
        let mut recovered = NativeSceneWorker::new(policy, || {
            checkpointed_program(
                71,
                protocol_digest(b"registered-counter-v1"),
                CHECKPOINT_WAIT_SCHEDULE,
            )
        })
        .unwrap();
        REGISTERED_CALLS.with(|calls| calls.set((0, 0)));
        recovered
            .handle(SupervisorRequest::RestoreCheckpoint(saved.clone()))
            .unwrap();
        assert_eq!(REGISTERED_CALLS.with(Cell::get), (0, 0));
        let restore_report = recovered.last_restore();
        // A budget of three refuses the four-frame suffix before execution.
        // A budget of four reaches the deliberately wrong *second* suffix hash.
        let attempted = if budget == 3 {
            journal_bytes.clone()
        } else {
            divergent.to_bytes().unwrap()
        };
        let error = recovered
            .handle(SupervisorRequest::ReplayJournal(JournalReplay {
                scene: NAME.into(),
                from_entry: 1,
                through_entry: 3,
                journal: attempted,
            }))
            .unwrap_err();
        assert_eq!(error.code, WorkerErrorCode::ReplayFailed);
        assert_eq!(
            REGISTERED_CALLS.with(Cell::get),
            if budget == 3 { (0, 0) } else { (4, 0) },
            "only the uncommitted suffix candidate may run",
        );
        assert_eq!(checkpoint_observation(&recovered), at_checkpoint);
        assert_eq!(recovered.journal_position(), 1);
        assert_eq!(recovered.last_state_hash(), Some(first.state_hash));
        assert_eq!(recovered.last_restore(), restore_report);
        assert!(recovered.journal_tail().is_empty());
        assert_eq!(png(&seek(&mut recovered, 600)), png(&checkpoint_png));

        if budget == 4 {
            REGISTERED_CALLS.with(|calls| calls.set((0, 0)));
            let response = recovered
                .handle(SupervisorRequest::ReplayJournal(JournalReplay {
                    scene: NAME.into(),
                    from_entry: 1,
                    through_entry: 3,
                    journal: journal_bytes.clone(),
                }))
                .unwrap();
            assert_eq!(
                response,
                WorkerResponse::ReplayComplete {
                    from_entry: 1,
                    state_hashes: vec![second.state_hash, third.state_hash],
                },
            );
            assert_eq!(REGISTERED_CALLS.with(Cell::get), (4, 0));
            assert_eq!(recovered.journal_position(), 3);
            assert_eq!(recovered.last_state_hash(), Some(third.state_hash));
            assert_eq!(checkpoint_observation(&recovered), expected_state);
            assert_eq!(png(&seek(&mut recovered, 604)), png(&expected));
        }
    }
}

#[test]
fn replay_suffix_executes_and_checks_every_actual_state_hash() {
    let mut original = worker();
    let first = commit(&mut original, 3);
    let second = commit(&mut original, 6);
    let mut journal = Journal::new();
    journal.record(first.clone()).unwrap();
    journal.record(second.clone()).unwrap();
    let mut recovered = worker();
    recovered
        .handle(SupervisorRequest::RestoreCheckpoint(checkpoint(&first, 0)))
        .unwrap();
    let response = recovered
        .handle(SupervisorRequest::ReplayJournal(JournalReplay {
            scene: NAME.into(),
            from_entry: 1,
            through_entry: 2,
            journal: journal.to_bytes().unwrap(),
        }))
        .unwrap();
    assert_eq!(
        response,
        WorkerResponse::ReplayComplete {
            from_entry: 1,
            state_hashes: vec![second.state_hash]
        }
    );
    assert_eq!(recovered.last_state_hash(), Some(second.state_hash));
    assert_eq!(recovered.program().unwrap().frame_index(), 6);
}

#[test]
fn forward_journal_reuses_one_candidate_without_repeating_stateful_frames() {
    let mut original = worker();
    let mut journal = Journal::new();
    let mut hashes = Vec::new();
    for frame in [2, 2, 5, 9] {
        let entry = commit(&mut original, frame);
        hashes.push(entry.state_hash);
        journal.record(entry).unwrap();
    }
    let expected = seek(&mut original, 12);
    let builds = Rc::new(Cell::new(0));
    let observed = Rc::clone(&builds);
    let mut policy = config();
    // Nine actual frame steps fit; rerunning every command from zero would
    // consume eighteen and fail this same production work budget.
    policy.max_replay_frames = 12;
    let mut recovered = NativeSceneWorker::new(policy, move || {
        observed.set(observed.get() + 1);
        program()
    })
    .unwrap();
    let before = builds.get();
    assert_eq!(
        recovered
            .handle(SupervisorRequest::ReplayJournal(JournalReplay {
                scene: NAME.into(),
                from_entry: 0,
                through_entry: 4,
                journal: journal.to_bytes().unwrap(),
            }))
            .unwrap(),
        WorkerResponse::ReplayComplete {
            from_entry: 0,
            state_hashes: hashes,
        }
    );
    assert_eq!(builds.get(), before + 1);
    assert_eq!(recovered.program().unwrap().frame_index(), 9);
    assert_eq!(png(&seek(&mut recovered, 12)), png(&expected));
    assert_eq!(
        builds.get(),
        before + 1,
        "continued playback keeps its callbacks"
    );
}

#[test]
fn backward_journal_reconstructs_a_new_candidate_and_charges_each_forward_run() {
    let mut original = worker();
    let mut journal = Journal::new();
    for frame in [2, 5, 1, 7] {
        journal.record(commit(&mut original, frame)).unwrap();
    }
    let expected = seek(&mut original, 7);
    for budget in [11, 12] {
        let builds = Rc::new(Cell::new(0));
        let observed = Rc::clone(&builds);
        let mut policy = config();
        policy.max_replay_frames = budget;
        let mut recovered = NativeSceneWorker::new(policy, move || {
            observed.set(observed.get() + 1);
            program()
        })
        .unwrap();
        let before = builds.get();
        let result = recovered.handle(SupervisorRequest::ReplayJournal(JournalReplay {
            scene: NAME.into(),
            from_entry: 0,
            through_entry: 4,
            journal: journal.to_bytes().unwrap(),
        }));
        if budget == 11 {
            assert_eq!(result.unwrap_err().code, WorkerErrorCode::ReplayFailed);
            assert_eq!(builds.get(), before, "work is refused before any callback");
            assert_eq!(recovered.journal_position(), 0);
        } else {
            result.unwrap();
            assert_eq!(builds.get(), before + 2, "one owner per monotonic run");
            assert_eq!(seek(&mut recovered, 7), expected);
        }
    }
}

#[test]
fn divergent_replay_installs_no_prefix_and_leaves_the_existing_owner_intact() {
    let mut original = worker();
    let first = commit(&mut original, 3);
    let mut second = commit(&mut original, 6);
    second.state_hash = protocol_digest(b"wrong state");
    second.checkpoint = None;
    let mut journal = Journal::new();
    journal.record(first).unwrap();
    journal.record(second).unwrap();
    let mut recovered = worker();
    let before = seek(&mut recovered, 2);
    let error = recovered
        .handle(SupervisorRequest::ReplayJournal(JournalReplay {
            scene: NAME.into(),
            from_entry: 0,
            through_entry: 2,
            journal: journal.to_bytes().unwrap(),
        }))
        .unwrap_err();
    assert_eq!(error.code, WorkerErrorCode::ReplayFailed);
    assert_eq!(recovered.program().unwrap().frame_index(), 2);
    assert!(recovered.journal_tail().is_empty());
    assert_eq!(png(&seek(&mut recovered, 2)), png(&before));
}

#[test]
fn changed_source_closure_and_aggregate_work_budget_refuse_before_factory_execution() {
    let mut original = worker();
    let first = commit(&mut original, 3);
    let second = commit(&mut original, 2);
    let mut journal = Journal::new();
    journal.record(first).unwrap();
    journal.record(second).unwrap();
    for changed_source in [false, true] {
        let builds = Rc::new(Cell::new(0));
        let observed = Rc::clone(&builds);
        let mut policy = config();
        if changed_source {
            policy.source_digest = protocol_digest(b"different source");
        } else {
            policy.max_replay_frames = 4; // three frames, then a fresh two-frame run
        }
        let mut recovered = NativeSceneWorker::new(policy, move || {
            observed.set(observed.get() + 1);
            program()
        })
        .unwrap();
        let count = builds.get();
        let result = recovered.handle(SupervisorRequest::ReplayJournal(JournalReplay {
            scene: NAME.into(),
            from_entry: 0,
            through_entry: 2,
            journal: journal.to_bytes().unwrap(),
        }));
        assert!(result.is_err());
        assert_eq!(builds.get(), count);
        assert_eq!(recovered.program().unwrap().frame_index(), 0);
    }
}

#[test]
fn unclassified_callbacks_are_opaque_and_cannot_be_restored_or_replayed() {
    let mut policy = config();
    policy.replay = NativeReplayPolicy::Disabled;
    let mut worker = NativeSceneWorker::new(policy, program).unwrap();
    let entry = commit(&mut worker, 3);
    assert_eq!(entry.effect, EffectClass::Opaque);
    assert_eq!(
        worker
            .handle(SupervisorRequest::RestoreCheckpoint(checkpoint(&entry, 0)))
            .unwrap_err()
            .code,
        WorkerErrorCode::CheckpointRejected
    );
    let mut journal = Journal::new();
    journal.record(entry).unwrap();
    assert_eq!(
        worker
            .handle(SupervisorRequest::ReplayJournal(JournalReplay {
                scene: NAME.into(),
                from_entry: 1,
                through_entry: 1,
                journal: journal.to_bytes().unwrap(),
            }))
            .unwrap_err()
            .code,
        WorkerErrorCode::ReplayFailed
    );
}

#[test]
fn bad_requests_and_forged_checkpoints_do_not_move_the_native_owner() {
    let mut worker = worker();
    let entry = commit(&mut worker, 3);
    let mut forged = checkpoint(&entry, 0);
    forged.state_hash = protocol_digest(b"forged");
    assert!(
        worker
            .handle(SupervisorRequest::RestoreCheckpoint(forged))
            .is_err()
    );
    assert!(
        worker
            .handle(SupervisorRequest::Scrub {
                scene: NAME.into(),
                frame: -1
            })
            .is_err()
    );
    assert!(
        worker
            .handle(SupervisorRequest::Scrub {
                scene: NAME.into(),
                frame: 13
            })
            .is_err()
    );
    assert_eq!(
        worker
            .handle(SupervisorRequest::Inspect {
                scene: "Other".into()
            })
            .unwrap_err()
            .code,
        WorkerErrorCode::SceneNotFound
    );
    assert_eq!(worker.program().unwrap().frame_index(), 3);
    assert_eq!(worker.last_state_hash(), Some(entry.state_hash));
}

#[test]
fn rehashed_rng_and_malformed_nested_snapshots_cannot_replace_live_callback_state() {
    use fmn_hash::{Limits, Reader, UnknownPolicy, Writer};
    use fmn_mobject::{SCENE_STATE_SCHEMA, SceneState, Stage};

    let mut original = worker();
    let entry = commit(&mut original, 3);
    let saved = checkpoint(&entry, 0);
    let mut reader = Reader::open(
        &saved.state,
        SCENE_STATE_SCHEMA,
        Limits::DEFAULT,
        UnknownPolicy::Strict,
    )
    .unwrap();
    let frames_elapsed = reader.get_i64().unwrap();
    let fps = reader.get_u32().unwrap();
    let play_count = reader.get_u64().unwrap();
    let mut rng_state = (
        [reader.get_u64().unwrap(), reader.get_u64().unwrap()],
        [reader.get_u64().unwrap(), reader.get_u64().unwrap()],
    );
    let snapshot = reader.get_bytes().unwrap();
    reader.finish().unwrap();
    rng_state.0[0] ^= 1;
    let encode = |snapshot: &[u8]| {
        let mut writer = Writer::new(SCENE_STATE_SCHEMA);
        writer
            .put_i64(frames_elapsed)
            .put_u32(fps)
            .put_u64(play_count);
        for word in [
            rng_state.0[0],
            rng_state.0[1],
            rng_state.1[0],
            rng_state.1[1],
        ] {
            writer.put_u64(word);
        }
        writer.put_bytes(snapshot);
        writer.finish().unwrap()
    };
    // Keep the original nested document, including updater identities. A
    // decoded arena cannot re-encode those until callbacks are rebound.
    let forged_rng = encode(snapshot);
    assert!(SceneState::from_bytes(&forged_rng, &Stage::new()).is_ok());
    let malformed_snapshot = encode(b"invalid nested snapshot");
    assert!(SceneState::from_bytes(&malformed_snapshot, &Stage::new()).is_err());

    for state in [forged_rng, malformed_snapshot] {
        assert_eq!(SceneState::frame_clock_from_bytes(&state).unwrap(), (3, 30));
        let mut recovered = worker();
        let before = seek(&mut recovered, 2);
        let error = recovered
            .handle(SupervisorRequest::RestoreCheckpoint(Checkpoint {
                state_hash: protocol_digest(&state),
                state,
                ..saved.clone()
            }))
            .unwrap_err();
        assert_eq!(error.code, WorkerErrorCode::CheckpointRejected);
        assert_eq!(recovered.journal_position(), 0);
        assert_eq!(recovered.program().unwrap().frame_index(), 2);
        assert_eq!(png(&seek(&mut recovered, 2)), png(&before));
    }
}

#[test]
fn inspector_and_overlay_respect_wire_limits_without_claiming_truncated_success() {
    let mut policy = config();
    policy.limits.max_studio_data_bytes = 16;
    let mut worker = NativeSceneWorker::new(policy, program).unwrap();
    assert!(
        worker
            .handle(SupervisorRequest::Inspect { scene: NAME.into() })
            .is_err()
    );
    assert!(
        worker
            .handle(SupervisorRequest::Overlay {
                scene: NAME.into(),
                layers: DebugLayerSet::ALL
            })
            .is_err()
    );
    assert_eq!(worker.program().unwrap().frame_index(), 0);
}

#[test]
fn handshake_and_requests_round_trip_through_the_actual_worker_pipe() {
    let limits = ProtocolLimits::default();
    let requests = [
        SupervisorRequest::Hello {
            version: CURRENT_VERSION,
            supervisor_build: protocol_digest(b"supervisor"),
            max_frame_bytes: 1_000_000,
        },
        SupervisorRequest::EnumerateScenes,
        SupervisorRequest::Scrub {
            scene: NAME.into(),
            frame: 3,
        },
        SupervisorRequest::Event {
            scene: NAME.into(),
            event: EventPayload::KeyPress {
                key: Key::Character('a'),
                modifiers: Modifiers::PRIMARY,
            },
        },
        SupervisorRequest::Inspect { scene: NAME.into() },
        SupervisorRequest::Shutdown,
    ];
    let mut input = Vec::new();
    for (index, request) in requests.into_iter().enumerate() {
        write_request(
            &mut input,
            &RequestEnvelope {
                request_id: u64::try_from(index).unwrap() + 1,
                request,
            },
            limits,
        )
        .unwrap();
    }
    let mut output = Vec::new();
    let mut worker = worker();
    let outcome = serve_worker(&mut worker, &mut Cursor::new(input), &mut output, limits).unwrap();
    assert!(matches!(outcome, WorkerServeOutcome::Shutdown));
    let mut output = Cursor::new(output);
    for index in 1..=6 {
        let envelope = read_response(&mut output, limits).unwrap();
        assert_eq!(envelope.request_id, index);
        match index {
            1 => assert!(matches!(envelope.response, WorkerResponse::Hello { .. })),
            2 => assert_eq!(envelope.response, WorkerResponse::Scenes(vec![NAME.into()])),
            3 | 4 => assert!(matches!(envelope.response, WorkerResponse::Frame(_))),
            5 => assert!(matches!(
                envelope.response,
                WorkerResponse::StudioData { .. }
            )),
            6 => assert_eq!(envelope.response, WorkerResponse::Bye),
            _ => unreachable!(),
        }
    }
}

#[test]
fn presenter_boundaries_survive_worker_protocol_and_cold_seek_without_extra_samples() {
    let mut policy = config();
    policy.presenter_mode = true;
    assert!(
        policy
            .base_input_reads()
            .iter()
            .any(|read| read.path == "native/presenter-policy")
    );
    let mut worker = NativeSceneWorker::new(policy, || {
        let scene = Scene::new(RuntimeConfig::default(), 71).unwrap();
        NativeSceneProgram::new(
            scene,
            vec![
                NativeSegment::Wait {
                    duration: Some(0.125),
                },
                NativeSegment::Wait {
                    duration: Some(0.25),
                },
            ],
            12,
        )
    })
    .unwrap();
    let limits = ProtocolLimits::default();
    let mut requests = vec![SupervisorRequest::Hello {
        version: CURRENT_VERSION,
        supervisor_build: protocol_digest(b"presenter-supervisor"),
        max_frame_bytes: 1_000_000,
    }];
    // A repeated inspect is a hold, and a backward seek reconstructs exactly
    // the same boundary. Explicit frame requests are the presenter's release.
    let states = [
        (0, true),
        (1, false),
        (4, true),
        (4, true),
        (5, false),
        (4, true),
        (12, true),
    ];
    for (frame, _) in states {
        requests.push(SupervisorRequest::Scrub {
            scene: NAME.into(),
            frame,
        });
        requests.push(SupervisorRequest::Inspect { scene: NAME.into() });
    }
    requests.push(SupervisorRequest::Shutdown);
    let mut input = Vec::new();
    for (index, request) in requests.into_iter().enumerate() {
        write_request(
            &mut input,
            &RequestEnvelope {
                request_id: index as u64 + 1,
                request,
            },
            limits,
        )
        .unwrap();
    }
    let mut output = Vec::new();
    assert!(matches!(
        serve_worker(&mut worker, &mut Cursor::new(input), &mut output, limits).unwrap(),
        WorkerServeOutcome::Shutdown
    ));
    let mut output = Cursor::new(output);
    assert!(matches!(
        read_response(&mut output, limits).unwrap().response,
        WorkerResponse::Hello { .. }
    ));
    for (frame, boundary) in states {
        let WorkerResponse::Frame(capture) = read_response(&mut output, limits).unwrap().response
        else {
            panic!("presenter continuation must produce a real capture");
        };
        assert_eq!(capture.frame_index, frame as u64);
        let WorkerResponse::StudioData { bytes, .. } =
            read_response(&mut output, limits).unwrap().response
        else {
            panic!("presenter inspection must remain responsive");
        };
        let json = String::from_utf8(bytes).unwrap();
        assert!(
            json.contains(&format!(
                "\"presenter\":{{\"enabled\":true,\"at_boundary\":{boundary}}}"
            )),
            "{json}"
        );
        assert!(
            json.contains(&format!("\"frame_index\":{frame},")),
            "{json}"
        );
    }
    assert_eq!(
        read_response(&mut output, limits).unwrap().response,
        WorkerResponse::Bye
    );
}
