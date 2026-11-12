//! Compiled frame jobs must preserve the existing reader, purity and clock.

use std::cell::Cell;
use std::rc::Rc;

use fmn_anim::{Timeline, prepare_animation};
use fmn_core::{rate, rng::RngRoot};
use fmn_mobject::animate::AnimateArgs;
use fmn_mobject::{Mobject, Stage};
use fmn_scene::timeline_bundle::{
    BundleReadError, BundleSegmentKind, SharedTimelineBundle, TimelineBundle, TimelineFrameCache,
    TimelineFrameJob, export_timeline_bundle,
};

fn export(rate_func: fn(f64) -> f64, duration: f64, fps: u32) -> Vec<u8> {
    let mut stage = Stage::new();
    let mob = stage.add(Mobject::from_points(&[[-1.0, 0.0, 0.0], [1.0, 0.0, 0.0]]));
    let tracker = stage.add_complex_value_tracker(2.0, -3.0);
    stage.add_many_to_scene(&[mob, tracker]).unwrap();
    let args = AnimateArgs {
        run_time: Some(duration),
        rate_func: Some(rate_func),
        ..AnimateArgs::default()
    };
    let builders = [
        mob.animate()
            .set_anim_args(args)
            .unwrap()
            .shift([2.0, 1.0, 0.0])
            .unwrap(),
        tracker
            .animate()
            .set_anim_args(args)
            .unwrap()
            .set_complex_value(6.0, 5.0)
            .unwrap(),
    ];
    let animations = builders
        .into_iter()
        .map(|builder| prepare_animation(builder, &mut stage).unwrap())
        .collect();
    let mut timeline = Timeline::new(fps).unwrap();
    timeline.play(animations).unwrap();
    export_timeline_bundle(timeline, &mut stage, &RngRoot::from_seed(7)).unwrap()
}

fn state(stage: &Stage) -> (Vec<u8>, u64) {
    (stage.snapshot().to_bytes().unwrap(), stage.time().to_bits())
}

#[test]
fn exact_frame_bundles_preserve_live_sampling_counts_and_shared_replay() {
    fn authored(fps: u32) -> (Stage, Timeline, fmn_mobject::Mob) {
        let mut stage = Stage::new();
        let mob = stage.add(Mobject::from_points(&[[0.0, 0.0, 0.0]]));
        stage.add_to_scene(mob).unwrap();
        let mut timeline = Timeline::new(fps).unwrap();
        let animation = prepare_animation(
            mob.animate()
                .set_anim_args(AnimateArgs {
                    run_time_frames: Some(43),
                    rate_func: Some(rate::linear),
                    ..AnimateArgs::default()
                })
                .unwrap()
                .shift([3.0, 0.0, 0.0])
                .unwrap(),
            &mut stage,
        )
        .unwrap();
        timeline.wait_frames(0).unwrap();
        timeline.play(vec![animation]).unwrap();
        timeline.wait_frames(17).unwrap();
        timeline.wait(0.25).unwrap();
        timeline.wait_frames(0).unwrap();
        (stage, timeline, mob)
    }
    for (fps, expected_frames) in [(24, 66), (25, 67), (30, 68), (60, 75)] {
        let (mut live_stage, mut live, mob) = authored(fps);
        let mut expected = Vec::new();
        live.render(&mut live_stage, &RngRoot::from_seed(10), &mut |packet| {
            let stage = packet.materialize_stage();
            expected.push((stage.get_center(mob), stage.time().to_bits()));
        })
        .unwrap();
        assert_eq!(expected.len(), expected_frames as usize);
        let (mut stage, timeline, _) = authored(fps);
        let bytes = export_timeline_bundle(timeline, &mut stage, &RngRoot::from_seed(10)).unwrap();
        let eager = TimelineBundle::from_bytes(&bytes).unwrap();
        let shared = SharedTimelineBundle::from_bytes(&bytes).unwrap();
        assert_eq!(eager.frame_count(), expected_frames);
        assert_eq!(shared.frame_count(), expected_frames);
        let mut cache = TimelineFrameCache::default();
        // Scrub in reverse order as well as reading the production order.
        for index in (0..expected_frames).rev().chain(0..expected_frames) {
            let eager_frame = eager.stage_at(index).unwrap();
            let shared_frame = cache
                .materialize(&shared.frame_job(index).unwrap())
                .unwrap();
            let expected = expected[index as usize];
            assert_eq!(
                (
                    eager_frame.get_center(eager_frame.roots()[0]),
                    eager_frame.time().to_bits()
                ),
                expected,
                "eager frame {index} at {fps} fps",
            );
            assert_eq!(
                (
                    shared_frame.get_center(shared_frame.roots()[0]),
                    shared_frame.time().to_bits()
                ),
                expected,
                "shared frame {index} at {fps} fps",
            );
        }
    }
}

#[test]
fn owned_jobs_are_send_sync_and_outlive_the_reader_in_reverse_parallel_order() {
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<SharedTimelineBundle>();
    assert_send_sync::<TimelineFrameJob>();
    for rate_func in [rate::linear as fn(f64) -> f64, rate::there_and_back] {
        let bytes = export(rate_func, 1.0, 8);
        let serial = TimelineBundle::from_bytes(&bytes).unwrap();
        let expected: Vec<_> = (0..serial.frame_count())
            .map(|index| state(&serial.stage_at(index).unwrap()))
            .collect();
        let kind = serial.segment_kind(0).unwrap();
        let shared = serial.into_shared().unwrap();
        assert_eq!(shared.segment_kind(0), Some(kind));
        let jobs: Vec<_> = (0..shared.frame_count())
            .rev()
            .map(|index| shared.frame_job(index).unwrap())
            .collect();
        drop(shared);
        drop(bytes);
        let handles: Vec<_> = jobs
            .into_iter()
            .map(|job| {
                std::thread::spawn(move || {
                    let mut cache = TimelineFrameCache::default();
                    let first = cache.materialize(&job).unwrap();
                    let before = state(&first);
                    let mut mutated = first;
                    mutated.shift(mutated.roots()[0], [999.0, 0.0, 0.0]);
                    assert_eq!(state(&cache.materialize(&job).unwrap()), before);
                    (job.index() as usize, before)
                })
            })
            .collect();
        for handle in handles {
            let (index, actual) = handle.join().unwrap();
            assert_eq!(actual, expected[index]);
        }
    }
}

#[test]
fn pure_endpoints_are_decoded_once_and_binary64_frame_boundaries_stay_exact() {
    for duration in [0.2, f64::from_bits(0.2f64.to_bits() - 1)] {
        let bytes = export(rate::linear, duration, 10);
        let serial = TimelineBundle::from_bytes(&bytes).unwrap();
        assert_eq!(serial.segment_kind(0), Some(BundleSegmentKind::Pure));
        assert_eq!(serial.frame_count(), if duration == 0.2 { 3 } else { 2 });
        let shared = SharedTimelineBundle::from_bytes(&bytes).unwrap();
        let mut cache = TimelineFrameCache::default();
        for _ in 0..3 {
            for index in (0..shared.frame_count()).rev() {
                assert_eq!(
                    state(
                        &cache
                            .materialize(&shared.frame_job(index).unwrap())
                            .unwrap()
                    ),
                    state(&serial.stage_at(index).unwrap())
                );
            }
        }
        assert_eq!(
            cache.decoded_snapshots(),
            2,
            "pure frames must not decode their endpoints repeatedly"
        );
        assert!(shared.snapshot_bytes() > 0);
    }
}

#[test]
fn dt_updaters_remain_recorded_and_are_never_reexecuted_by_workers() {
    let mut stage = Stage::new();
    let tracker = stage.add_value_tracker(0.0);
    stage.add_to_scene(tracker).unwrap();
    let count = Rc::new(Cell::new(0));
    let seen = count.clone();
    stage
        .add_dt_updater(
            tracker,
            move |stage, mob, dt| {
                if dt > 0.0 {
                    seen.set(seen.get() + 1);
                }
                let value = stage.tracker_value(mob).unwrap();
                stage.set_tracker_value(mob, value + dt).unwrap();
            },
            false,
        )
        .unwrap();
    let mut timeline = Timeline::new(8).unwrap();
    timeline.wait(0.5).unwrap();
    let bytes = export_timeline_bundle(timeline, &mut stage, &RngRoot::from_seed(7)).unwrap();
    assert_eq!(count.get(), 4);
    let serial = TimelineBundle::from_bytes(&bytes).unwrap();
    assert_eq!(serial.segment_kind(0), Some(BundleSegmentKind::Stateful));
    let shared = SharedTimelineBundle::from_bytes(&bytes).unwrap();
    let mut cache = TimelineFrameCache::default();
    for index in [3, 0, 2, 1] {
        let job = shared.frame_job(index).unwrap();
        assert_eq!(job.kind(), BundleSegmentKind::Stateful);
        assert_eq!(
            state(&cache.materialize(&job).unwrap()),
            state(&serial.stage_at(index).unwrap())
        );
    }
    assert_eq!(cache.decoded_snapshots(), 4);
    assert_eq!(count.get(), 4);
}

#[test]
fn malformed_inputs_and_out_of_range_jobs_keep_the_readers_refusal() {
    let mut bytes = export(rate::linear, 0.5, 8);
    let shared = SharedTimelineBundle::from_bytes(&bytes).unwrap();
    for index in [shared.frame_count(), u32::MAX] {
        assert!(matches!(
            shared.frame_job(index),
            Err(BundleReadError::FrameOutOfRange { .. })
        ));
    }
    let last = bytes.len() - 1;
    bytes[last] ^= 1;
    assert!(TimelineBundle::from_bytes(&bytes).is_err());
    assert!(SharedTimelineBundle::from_bytes(&bytes).is_err());
    let mut stage = Stage::new();
    let bytes = export_timeline_bundle(
        Timeline::new(8).unwrap(),
        &mut stage,
        &RngRoot::from_seed(0),
    )
    .unwrap();
    let empty = SharedTimelineBundle::from_bytes(&bytes).unwrap();
    assert_eq!(empty.frame_count(), 0);
    assert_eq!(empty.snapshot_bytes(), 0);
    assert!(empty.frame_job(0).is_err());
}

#[test]
fn checked_in_demo_replays_without_retagging_decoded_function_pointers() {
    // This is the actual README/browser asset, not a freshly exported file
    // that might accidentally carry this build's function-address choices.
    let bytes = include_bytes!("../../../demo/wasm/bundle.fmtl");
    let eager = TimelineBundle::from_bytes(bytes).unwrap();
    assert_eq!(eager.frame_count(), 45);
    let shared = SharedTimelineBundle::from_bytes(bytes).unwrap();
    let mut cache = TimelineFrameCache::default();
    for index in (0..45).rev().chain(0..45) {
        assert_eq!(
            state(
                &cache
                    .materialize(&shared.frame_job(index).unwrap())
                    .unwrap()
            ),
            state(&eager.stage_at(index).unwrap()),
            "README bundle frame {index}",
        );
    }
}

#[test]
fn every_catalog_export_round_trips_through_eager_and_shared_readers() {
    // Address acquisition, nomination, encoding, decoding and shared replay
    // cross several crates. Run this under --release as well as debug.
    for function in [
        rate::linear as fn(f64) -> f64,
        rate::smooth,
        rate::rush_into,
        rate::rush_from,
        rate::slow_into,
        rate::double_smooth,
        rate::there_and_back,
        rate::lingering,
    ] {
        let bytes = export(function, 1.0, 8);
        assert_eq!(bytes, export(function, 1.0, 8));
        let eager = TimelineBundle::from_bytes(&bytes).unwrap();
        let shared = SharedTimelineBundle::from_bytes(&bytes).unwrap();
        assert_eq!(shared.frame_count(), 8);
        assert_eq!(shared.segment_kind(0), eager.segment_kind(0));
        let mut cache = TimelineFrameCache::default();
        for index in [7, 0, 3, 1, 6, 2, 5, 4, 0] {
            assert_eq!(
                state(
                    &cache
                        .materialize(&shared.frame_job(index).unwrap())
                        .unwrap()
                ),
                state(&eager.stage_at(index).unwrap()),
            );
        }
    }
    // Matching endpoint values do not make an arbitrary function a known
    // catalog rate. Preserve the writer's fail-closed stateful fallback.
    fn custom(alpha: f64) -> f64 {
        alpha * alpha
    }
    let bytes = export(custom, 1.0, 8);
    let shared = SharedTimelineBundle::from_bytes(&bytes).unwrap();
    assert_eq!(shared.segment_kind(0), Some(BundleSegmentKind::Stateful));
}

#[test]
fn compact_static_jobs_keep_blocks_compressed_and_reuse_decoded_bases() {
    use fmn_scene::recording::{CompactRecordingLimits, SceneBundleRecorder};
    use fmn_scene::{RuntimeConfig, Scene};

    let mut scene = Scene::new(RuntimeConfig::default(), 17).unwrap();
    for row in 0..24 {
        let points = (0..96)
            .map(|column| [f64::from(column) / 16.0, f64::from(row), 0.0])
            .collect::<Vec<_>>();
        scene.add_mobject(Mobject::from_points(&points)).unwrap();
    }
    let expected = scene.stage().snapshot().to_render_bytes().unwrap();
    let mut recorder =
        SceneBundleRecorder::new_compact(scene.fps(), CompactRecordingLimits::default()).unwrap();
    scene.wait_frames(130, &mut recorder).unwrap();
    let artifact = recorder.finish().unwrap();
    assert!(artifact.bytes.len() * 8 < expected.len() * 130);
    let bundle = SharedTimelineBundle::from_bytes(&artifact.bytes).unwrap();
    assert_eq!(
        bundle.segment_kind(0),
        Some(BundleSegmentKind::CompactRecorded)
    );
    assert!(bundle.snapshot_bytes() * 8 < expected.len() * 130);
    let jobs = (0..130)
        .map(|index| bundle.frame_job(index).unwrap())
        .collect::<Vec<_>>();
    drop(bundle);
    let mut cache = TimelineFrameCache::default();
    for job in &jobs {
        let frame = cache.materialize(job).unwrap();
        assert_eq!(frame.snapshot().to_render_bytes().unwrap(), expected);
        assert_eq!(
            frame.time().to_bits(),
            ((f64::from(job.index()) + 1.0) / f64::from(scene.fps())).to_bits(),
        );
    }
    assert_eq!(
        cache.decoded_snapshots(),
        3,
        "one decode for each 64-frame block"
    );
}

#[test]
fn compact_stateful_jobs_outlive_the_reader_and_match_observed_frames_in_parallel() {
    use fmn_anim::FramePacket;
    use fmn_scene::recording::{CompactRecordingLimits, SceneBundleRecorder};
    use fmn_scene::{
        CaptureReason, IntegrationError, LifecycleEvent, RuntimeConfig, Scene, SceneSink,
    };

    struct Oracle {
        recorder: SceneBundleRecorder,
        observed: Vec<Vec<u8>>,
    }
    impl SceneSink for Oracle {
        fn event(&mut self, event: LifecycleEvent) -> Result<(), IntegrationError> {
            self.recorder.event(event)
        }
        fn capture(
            &mut self,
            reason: CaptureReason,
            packet: FramePacket,
        ) -> Result<(), IntegrationError> {
            self.observed
                .push(packet.state().to_render_bytes().unwrap());
            self.recorder.capture(reason, packet)
        }
    }
    let mut scene = Scene::new(RuntimeConfig::default(), 19).unwrap();
    let moving = scene
        .add_mobject(Mobject::from_points(&[[0.0, 1.0, 0.0]]))
        .unwrap();
    let background = (0..256)
        .map(|index| [f64::from(index), -2.0, 0.0])
        .collect::<Vec<_>>();
    scene
        .add_mobject(Mobject::from_points(&background))
        .unwrap();
    let calls = Rc::new(Cell::new(0u32));
    let counter = Rc::clone(&calls);
    scene
        .stage_mut()
        .add_dt_updater(
            moving,
            move |stage, mob, _dt| {
                let next = counter.get() + 1;
                counter.set(next);
                stage.set_x(mob, f64::from(next * next) / 1024.0);
            },
            false,
        )
        .unwrap();
    let mut oracle = Oracle {
        recorder: SceneBundleRecorder::new_compact(scene.fps(), CompactRecordingLimits::default())
            .unwrap(),
        observed: Vec::new(),
    };
    scene.wait_frames(130, &mut oracle).unwrap();
    let callback_count = calls.get();
    assert_ne!(oracle.observed[0], oracle.observed[129]);
    let artifact = oracle.recorder.finish().unwrap();
    let bundle = SharedTimelineBundle::from_bytes(&artifact.bytes).unwrap();
    let jobs = (0..130)
        .rev()
        .map(|index| bundle.frame_job(index).unwrap())
        .collect::<Vec<_>>();
    drop(bundle);
    let frames = std::thread::scope(|scope| {
        let workers = jobs
            .chunks(33)
            .map(|chunk| {
                scope.spawn(move || {
                    let mut cache = TimelineFrameCache::default();
                    chunk
                        .iter()
                        .map(|job| {
                            assert_eq!(job.kind(), BundleSegmentKind::CompactRecorded);
                            let frame = cache.materialize(job).unwrap();
                            (job.index(), frame.snapshot().to_render_bytes().unwrap())
                        })
                        .collect::<Vec<_>>()
                })
            })
            .collect::<Vec<_>>();
        workers
            .into_iter()
            .flat_map(|worker| worker.join().unwrap())
            .collect::<Vec<_>>()
    });
    assert_eq!(frames.len(), 130);
    for (index, bytes) in frames {
        assert_eq!(bytes, oracle.observed[index as usize], "frame {index}");
    }
    assert_eq!(
        calls.get(),
        callback_count,
        "playback must never invoke an updater"
    );
}
