//! Declarative native target callbacks are evaluated at export, never replayed.
use std::cell::Cell;
use std::rc::Rc;

use fmn_anim::{AnimConfig, ApplyMethod, RateFunc, Timeline};
use fmn_core::rng::RngRoot;
use fmn_mobject::{Mobject, Stage};
use fmn_scene::timeline_bundle::{
    BundleSegmentKind, SharedTimelineBundle, TimelineBundle, TimelineFrameCache,
    export_timeline_bundle,
};

#[test]
fn deferred_target_exports_actual_frames_and_releases_callbacks_before_worker_replay() {
    let mut stage = Stage::new();
    let source = stage.add(Mobject::from_points(&[[1.0, 0.0, 0.0]]));
    stage.add_to_scene(source).unwrap();
    let calls = Rc::new(Cell::new(0));
    let capture = Rc::clone(&calls);
    let animation = ApplyMethod::new(source, move |s, target| {
        capture.set(capture.get() + 1);
        let position = s.get_center(target);
        s.move_to(
            target,
            [position[0] * position[0], position[1], position[2]],
            [0.0; 3],
        );
        Ok(())
    })
    .with_config(AnimConfig {
        run_time: 1.0,
        rate_func: RateFunc::linear(),
        ..AnimConfig::default()
    });
    let mut timeline = Timeline::new(4).unwrap();
    timeline.play(vec![Box::new(animation)]).unwrap();
    stage.shift(source, [2.0, 0.0, 0.0]);
    assert_eq!(calls.get(), 0);
    let bytes = export_timeline_bundle(timeline, &mut stage, &RngRoot::from_seed(7)).unwrap();
    assert_eq!(calls.get(), 1);
    assert_eq!(Rc::strong_count(&calls), 1);
    let eager = TimelineBundle::from_bytes(&bytes).unwrap();
    assert_eq!(eager.segment_kind(0), Some(BundleSegmentKind::Stateful));
    let states: Vec<_> = (0..4)
        .map(|index| {
            let frame = eager.stage_at(index).unwrap();
            let expected = 3.0 + 6.0 * f64::from(index + 1) / 4.0;
            assert_eq!(frame.get_center(frame.roots()[0]), [expected, 0.0, 0.0]);
            frame.snapshot_bytes().unwrap()
        })
        .collect();
    let shared = SharedTimelineBundle::from_bytes(&bytes).unwrap();
    for workers in [1, 4, 16] {
        let inputs: Vec<_> = (0..4)
            .rev()
            .chain(0..4)
            .map(|index| shared.frame_job(index).unwrap())
            .collect();
        let handles: Vec<_> = (0..workers)
            .map(|worker| {
                let jobs: Vec<_> = inputs
                    .iter()
                    .enumerate()
                    .filter(|(ordinal, _)| ordinal % workers == worker)
                    .map(|(_, job)| job.clone())
                    .collect();
                let expected = states.clone();
                std::thread::spawn(move || {
                    let mut cache = TimelineFrameCache::default();
                    for job in jobs {
                        assert_eq!(
                            cache.materialize(&job).unwrap().snapshot_bytes().unwrap(),
                            expected[job.index() as usize]
                        );
                    }
                })
            })
            .collect();
        for handle in handles {
            handle.join().unwrap();
        }
    }
    assert_eq!(calls.get(), 1);
}
