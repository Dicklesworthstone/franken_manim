//! Native timeline jumps keep real executable continuations and verified recovery.

use std::cell::Cell;
use std::rc::Rc;

use fmn_anim::{AnimConfig, RateFunc, Transform};
use fmn_core::color::LinearRgba;
use fmn_mobject::{Mob, Mobject, RecordBuffer, RecordSchema};
use fmn_render::{
    CameraConfig, EngineIdentity, FrameConfig, RetainedFrameRendererConfig, ScreenMap, Tiling,
    Viewport,
};
use fmn_scene::{CameraRig, EventPayload, Modifiers, PlayOverrides, RuntimeConfig, Scene};
use fmn_studio::native::{
    NativeReplayPolicy, NativeSceneProgram, NativeSceneWorker, NativeSeekWork, NativeSegment,
    NativeWorkerConfig,
};
use fmn_studio::{
    Checkpoint, FramePayload, ServiceError, SupervisorRequest, WorkerResponse, WorkerService,
    protocol_digest,
};

const NAME: &str = "PureSeek";
const PLAY_FRAMES: u64 = 1_800;
const TOTAL_FRAMES: u64 = PLAY_FRAMES + 60;

fn initial() -> (Scene, Mob) {
    let mut scene = Scene::new(
        RuntimeConfig {
            fps: 60,
            ..RuntimeConfig::default()
        },
        71,
    )
    .unwrap();
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
    let root = scene.add_mobject(Mobject::from_buffer(buffer)).unwrap();
    (scene, root)
}

fn program(calls: Option<Rc<Cell<u64>>>) -> Result<NativeSceneProgram, ServiceError> {
    let (mut scene, root) = initial();
    if let Some(calls) = calls {
        scene
            .stage_mut()
            .add_updater(root, move |_, _| calls.set(calls.get() + 1), false)
            .unwrap();
    }
    let target = scene.stage_mut().copy_family(root).unwrap();
    scene.stage_mut().shift(target, [3.0, 1.0, 0.0]);
    let animation = Transform::new(root, target).with_config(AnimConfig {
        rate_func: RateFunc::linear(),
        suspend_mobject_updating: false,
        ..AnimConfig::default()
    });
    NativeSceneProgram::new(
        scene,
        vec![
            NativeSegment::Play {
                animations: vec![Box::new(animation)],
                overrides: PlayOverrides {
                    run_time_frames: Some(PLAY_FRAMES),
                    ..PlayOverrides::default()
                },
            },
            NativeSegment::WaitFrames { frames: 60 },
        ],
        TOTAL_FRAMES,
    )
}

fn serial_to(program: &mut NativeSceneProgram, target: u64) {
    while program.frame_index() < target {
        assert!(program.next_frame().unwrap().is_some());
    }
}

fn same_state(actual: &mut NativeSceneProgram, expected: &mut NativeSceneProgram) {
    assert_eq!(actual.frame_index(), expected.frame_index());
    assert_eq!(actual.at_segment_boundary(), expected.at_segment_boundary());
    assert_eq!(
        actual.state_bytes().unwrap(),
        expected.state_bytes().unwrap()
    );
}

#[test]
fn pure_jumps_match_serial_captures_and_keep_the_next_frame_executable() {
    let mut actual = program(None).unwrap();
    let mut serial = program(None).unwrap();
    for target in [600, 1_200, 1_799, 1_800, 1_830, TOTAL_FRAMES] {
        let advanced = target - actual.frame_index();
        let work = actual.advance_to_with_work(target).unwrap();
        assert_eq!(
            work,
            NativeSeekWork {
                frames_advanced: advanced,
                frames_evaluated: u64::from(advanced != 0),
            },
            "a pure interval evaluates only its requested final sample"
        );
        serial_to(&mut serial, target);
        same_state(&mut actual, &mut serial);
        if target < TOTAL_FRAMES {
            let actual_frame = actual.next_frame().unwrap().unwrap();
            let expected_frame = serial.next_frame().unwrap().unwrap();
            assert_eq!(actual_frame.time(), expected_frame.time());
            assert_eq!(actual_frame.alpha(), expected_frame.alpha());
            assert_eq!(actual_frame.segment_frame(), expected_frame.segment_frame());
            assert_eq!(
                actual_frame
                    .materialize_stage()
                    .snapshot()
                    .to_bytes()
                    .unwrap(),
                expected_frame
                    .materialize_stage()
                    .snapshot()
                    .to_bytes()
                    .unwrap()
            );
            same_state(&mut actual, &mut serial);
        }
    }
    assert!(actual.advance_to(TOTAL_FRAMES + 1).is_err());
    same_state(&mut actual, &mut serial);
}

#[test]
fn input_is_a_barrier_for_the_open_segment_but_not_for_the_next_one() {
    let mut actual = program(None).unwrap();
    let mut serial = program(None).unwrap();
    assert_eq!(
        actual.advance_to_with_work(100).unwrap().frames_evaluated,
        1
    );
    serial_to(&mut serial, 100);
    let event = EventPayload::MouseMotion {
        point: [20.0, 20.0, 0.0],
        delta: [0.0; 3],
        modifiers: Modifiers::NONE,
    };
    actual.dispatch(event.clone()).unwrap();
    serial.dispatch(event).unwrap();
    assert_eq!(
        actual.advance_to_with_work(200).unwrap().frames_evaluated,
        100
    );
    serial_to(&mut serial, 200);
    same_state(&mut actual, &mut serial);
    actual.advance_to(PLAY_FRAMES).unwrap();
    serial_to(&mut serial, PLAY_FRAMES);
    same_state(&mut actual, &mut serial);
    assert_eq!(
        actual.advance_to_with_work(PLAY_FRAMES + 40).unwrap(),
        NativeSeekWork {
            frames_advanced: 40,
            frames_evaluated: 1,
        }
    );
    serial_to(&mut serial, PLAY_FRAMES + 40);
    same_state(&mut actual, &mut serial);
}

#[test]
fn mutable_updater_captures_still_execute_at_every_ordinary_boundary() {
    let calls = Rc::new(Cell::new(0));
    let expected_calls = Rc::new(Cell::new(0));
    let mut actual = program(Some(Rc::clone(&calls))).unwrap();
    let mut serial = program(Some(Rc::clone(&expected_calls))).unwrap();
    assert_eq!(
        actual.advance_to_with_work(200).unwrap(),
        NativeSeekWork {
            frames_advanced: 200,
            frames_evaluated: 200,
        }
    );
    serial_to(&mut serial, 200);
    same_state(&mut actual, &mut serial);
    assert_eq!(calls.get(), expected_calls.get());
    assert!(calls.get() >= 200);
}

#[test]
fn camera_validation_still_refuses_an_invalid_intermediate_sample() {
    let (mut scene, _) = initial();
    let rig = CameraRig::new(&mut scene, &CameraConfig::default()).unwrap();
    let target = scene.stage_mut().copy_family(rig.width()).unwrap();
    scene.stage_mut().set_tracker_value(target, 0.0).unwrap();
    let animation = Transform::new(rig.width(), target).with_config(AnimConfig {
        // The width reaches zero halfway through and is positive again at
        // the requested final nominal sample. Seeking must observe the zero.
        rate_func: RateFunc::Sampled(std::sync::Arc::from([0.0, 1.0, 0.0])),
        ..AnimConfig::default()
    });
    let mut program = NativeSceneProgram::new(
        scene,
        vec![NativeSegment::Play {
            animations: vec![Box::new(animation)],
            overrides: PlayOverrides {
                run_time_frames: Some(120),
                ..PlayOverrides::default()
            },
        }],
        120,
    )
    .unwrap()
    .with_camera_rig(rig)
    .unwrap();
    assert!(program.advance_to(120).is_err());
    assert!(program.frame_index() < 120);
    assert!(
        program.state_bytes().is_err(),
        "failed camera poisons the owner"
    );
}

fn deferred_program(builds: Rc<Cell<u64>>) -> NativeSceneProgram {
    let (scene, root) = initial();
    NativeSceneProgram::new(
        scene,
        vec![
            NativeSegment::WaitFrames { frames: 120 },
            NativeSegment::defer(move |context| {
                assert_eq!(context.scene().time().frames(), 120);
                assert_eq!(context.scene().play_count(), 1);
                builds.set(builds.get() + 1);
                context.stage_mut().shift(root, [1.0, 0.0, 0.0]);
                Ok(vec![NativeSegment::WaitFrames { frames: 60 }])
            }),
        ],
        180,
    )
    .unwrap()
}

#[test]
fn pure_wait_jumps_leave_epilogues_and_deferred_construction_pending() {
    let builds = Rc::new(Cell::new(0));
    let serial_builds = Rc::new(Cell::new(0));
    let mut actual = deferred_program(Rc::clone(&builds));
    let mut serial = deferred_program(Rc::clone(&serial_builds));
    assert_eq!(
        actual.advance_to_with_work(120).unwrap().frames_evaluated,
        1
    );
    serial_to(&mut serial, 120);
    same_state(&mut actual, &mut serial);
    assert!(actual.at_segment_boundary());
    assert_eq!(actual.preview().scene().play_count(), 0);
    assert_eq!(builds.get(), 0);
    assert_eq!(
        actual.advance_to_with_work(150).unwrap().frames_evaluated,
        1
    );
    serial_to(&mut serial, 150);
    same_state(&mut actual, &mut serial);
    assert_eq!(builds.get(), 1);
    assert_eq!(builds.get(), serial_builds.get());
}

fn worker_config() -> NativeWorkerConfig {
    let renderer = RetainedFrameRendererConfig {
        frame: FrameConfig::new(
            Viewport {
                width: 64,
                height: 64,
            },
            ScreenMap {
                scale: 8.0,
                origin: [24.0, 32.0],
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
        protocol_digest(b"pure-seek-build"),
        protocol_digest(b"pure-seek-source"),
        TOTAL_FRAMES + 1,
        60,
        renderer,
    );
    config.replay = NativeReplayPolicy::ColdVerified;
    config
}

fn png_at(worker: &mut NativeSceneWorker, frame: i64) -> Vec<u8> {
    let WorkerResponse::Frame(frame) = worker
        .handle(SupervisorRequest::Scrub {
            scene: NAME.into(),
            frame,
        })
        .unwrap()
    else {
        panic!("expected a native frame")
    };
    let FramePayload::Pipe { bytes, .. } = frame.payload else {
        panic!("expected canonical PNG bytes")
    };
    bytes
}

#[test]
fn a_serial_twenty_second_checkpoint_restores_and_continues_with_identical_pngs() {
    let mut serial = program(None).unwrap();
    serial_to(&mut serial, 1_200);
    let state = serial.state_bytes().unwrap();
    let state_hash = protocol_digest(&state);
    let mut actual = NativeSceneWorker::new(worker_config(), || program(None)).unwrap();
    let response = actual
        .handle(SupervisorRequest::RestoreCheckpoint(Checkpoint {
            scene: NAME.into(),
            after_entry: 0,
            state,
            state_hash,
        }))
        .unwrap();
    assert_eq!(
        response,
        WorkerResponse::Ack {
            state_hash: Some(state_hash),
            journal_len: 1
        }
    );
    assert_eq!(actual.program().unwrap().frame_index(), 1_200);

    // The comparison owner has an active no-op updater, so its native seek
    // cannot use pure sampling. Its geometry and pixels are otherwise equal.
    let mut reference =
        NativeSceneWorker::new(worker_config(), || program(Some(Rc::new(Cell::new(0))))).unwrap();
    for frame in [1_201, 1_202, 1_700, 1_800, 1_801, 1_830] {
        assert_eq!(png_at(&mut actual, frame), png_at(&mut reference, frame));
    }
}
