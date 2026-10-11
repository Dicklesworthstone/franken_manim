//! FMTL export must preserve admitted tags, not silently record known curves.
//! Run this integration crate in release as well as debug.

use fmn_anim::{Timeline, prepare_animation};
use fmn_core::{rate, rng::RngRoot};
use fmn_mobject::animate::AnimateArgs;
use fmn_mobject::{Mobject, Stage};
use fmn_scene::timeline_bundle::{
    BundleSegmentKind, SharedTimelineBundle, TIMELINE_BUNDLE_SCHEMA, TimelineBundle,
    TimelineFrameCache, bundle_engine_version, export_timeline_bundle,
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
fn nominated_waits_remain_pure_across_zero_length_and_empty_stages() {
    for with_tracker in [false, true] {
        let mut stage = Stage::new();
        if with_tracker {
            let tracker = stage.add_value_tracker(7.0);
            stage.add_to_scene(tracker).unwrap();
        }
        let mut timeline = Timeline::new(8).unwrap();
        timeline.wait(0.0).unwrap();
        timeline.wait(0.5).unwrap();
        let bytes = export_timeline_bundle(timeline, &mut stage, &RngRoot::from_seed(9)).unwrap();
        let eager = TimelineBundle::from_bytes(&bytes).unwrap();
        assert_eq!(eager.frame_count(), 4);
        for segment in 0..2 {
            assert_eq!(eager.segment_kind(segment), Some(BundleSegmentKind::Pure));
        }
        let expected: Vec<_> = (0..4)
            .map(|index| state(&eager.stage_at(index).unwrap()))
            .collect();
        let shared = eager.into_shared().unwrap();
        let mut cache = TimelineFrameCache::default();
        for index in [3, 0, 2, 1] {
            assert_eq!(
                state(
                    &cache
                        .materialize(&shared.frame_job(index).unwrap())
                        .unwrap()
                ),
                expected[index as usize]
            );
        }
        assert_eq!(cache.decoded_snapshots(), 2);
    }
}

#[test]
fn native_catalog_admission_preserves_all_wire_tags_including_stationary_loops() {
    use fmn_hash::serial::{Limits, Reader, UnknownPolicy};

    let functions: [fn(f64) -> f64; 8] = [
        rate::linear,
        rate::smooth,
        rate::rush_into,
        rate::rush_from,
        rate::slow_into,
        rate::double_smooth,
        rate::there_and_back,
        rate::lingering,
    ];
    for (tag, func) in functions.into_iter().enumerate() {
        // A constant tracker is pure even for there_and_back. A nonconstant
        // out-and-back animation must still fail the endpoint proof; the
        // existing tracker_bundle regression separately checks that refusal.
        let mut stage = Stage::new();
        let tracker = stage.add_value_tracker(0.0);
        stage.add_to_scene(tracker).unwrap();
        let builder = tracker
            .animate()
            .set_anim_args(AnimateArgs {
                run_time: Some(1.0),
                rate_func: Some(std::hint::black_box(func)),
                ..AnimateArgs::default()
            })
            .unwrap()
            .set_value(0.0)
            .unwrap();
        let mut timeline = Timeline::new(8).unwrap();
        timeline
            .play(vec![prepare_animation(builder, &mut stage).unwrap()])
            .unwrap();
        let bytes = export_timeline_bundle(timeline, &mut stage, &RngRoot::from_seed(7)).unwrap();
        // Inspect the wire tag, not just the numerically constant playback.
        // Recorded fallback or a hard-coded linear tag would otherwise pass.
        let mut wire = Reader::open(
            &bytes,
            TIMELINE_BUNDLE_SCHEMA,
            Limits::DEFAULT,
            UnknownPolicy::Strict,
        )
        .unwrap();
        assert_eq!(wire.get_str().unwrap(), bundle_engine_version());
        assert_eq!(wire.get_u32().unwrap(), 8);
        wire.get_bytes().unwrap(); // validated nested plan
        assert_eq!(wire.get_u32().unwrap(), 1);
        assert_eq!(wire.get_u8().unwrap(), 0, "rate {tag} became recorded");
        wire.get_bytes().unwrap(); // begin
        wire.get_bytes().unwrap(); // end
        assert_eq!(wire.get_u8().unwrap(), 0);
        assert_eq!(usize::from(wire.get_u8().unwrap()), tag);
        wire.finish().unwrap();
        let shared = SharedTimelineBundle::from_bytes(&bytes).unwrap();
        assert_eq!(shared.segment_kind(0), Some(BundleSegmentKind::Pure));
        assert_eq!(shared.frame_count(), 8);
        for index in [7, 0, 3, 6] {
            let frame = shared.frame_job(index).unwrap().materialize().unwrap();
            assert_eq!(frame.tracker_value(frame.roots()[0]), Some(0.0));
        }
    }
}

#[test]
fn unrecognized_native_rate_exports_recorded_frames_without_losing_motion() {
    let bytes = export(|t| t * t, 1.0, 8);
    let eager = TimelineBundle::from_bytes(&bytes).unwrap();
    assert_eq!(eager.segment_kind(0), Some(BundleSegmentKind::Stateful));
    let expected: Vec<_> = (0..8)
        .map(|index| state(&eager.stage_at(index).unwrap()))
        .collect();
    let shared = eager.into_shared().unwrap();
    for index in [7, 0, 3, 6, 1, 5, 2, 4] {
        let stage = shared.frame_job(index).unwrap().materialize().unwrap();
        let t = f64::from(index + 1) / 8.0;
        let alpha = t * t;
        let real = (1.0 - alpha) * 2.0 + alpha * 6.0;
        let imaginary = (1.0 - alpha) * -3.0 + alpha * 5.0;
        assert_eq!(
            stage.tracker_complex_value(stage.roots()[1]),
            Some((real, imaginary))
        );
        assert_eq!(state(&stage), expected[index as usize]);
    }
}
