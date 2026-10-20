//! Broadcast's options must drive its actual timeline and cleanup, not just metadata.
use fmn_anim::{AnimConfig, Animation, RateFunc, broadcast, restore};
use fmn_mobject::{Mob, Mobject, Stage};

fn circles(stage: &mut Stage) -> (Vec<Mob>, Vec<Box<dyn Animation>>) {
    let mut handles = Vec::new();
    let mut animations: Vec<Box<dyn Animation>> = Vec::new();
    for (index, duration) in [0.25, 0.5, 1.0].into_iter().enumerate() {
        let mob = stage.add(Mobject::from_points(&[[index as f64 * 10.0, 0.0, 0.0]]));
        stage.add_to_scene(mob).unwrap();
        stage.save_state(mob).unwrap();
        stage.shift(mob, [1.0, 0.0, 0.0]);
        let animation = restore(stage, mob).unwrap().with_config(AnimConfig {
            run_time: duration,
            rate_func: RateFunc::linear(),
            ..AnimConfig::default()
        });
        handles.push(mob);
        animations.push(Box::new(animation));
    }
    (handles, animations)
}

#[test]
fn broadcast_uses_authored_lag_for_intervals_and_actual_motion() {
    for lag in [0.0, 0.2, 0.5, 1.0] {
        let mut stage = Stage::new();
        let (handles, animations) = circles(&mut stage);
        let mut group = broadcast(&mut stage, animations, 3.0, lag, false).unwrap();
        let starts = [0.0, 0.25 * lag, 0.25 * lag + 0.5 * lag];
        let durations = [0.25, 0.5, 1.0];
        for ((interval, start), duration) in group.timings().iter().zip(starts).zip(durations) {
            assert!((interval.start - start).abs() < 1e-14);
            assert!((interval.end - start - duration).abs() < 1e-14);
        }
        let end = starts[2] + durations[2];
        assert!((group.max_end_time() - end).abs() < 1e-14);
        group.begin(&mut stage).unwrap();
        for alpha in [0.0, 0.1, 0.5, 0.9, 1.0] {
            group.interpolate(&mut stage, alpha);
            for (i, mob) in handles.iter().enumerate() {
                let progress = ((alpha * end - starts[i]) / durations[i]).clamp(0.0, 1.0);
                let expected = i as f64 * 10.0 + 1.0 - progress;
                assert!(
                    (stage.get_center(*mob)[0] - expected).abs() < 1e-12,
                    "lag={lag}, alpha={alpha}, member={i}"
                );
            }
        }
        group.finish(&mut stage);
    }
}

#[test]
fn broadcast_removes_its_members_only_when_requested() {
    for remover in [false, true] {
        let mut stage = Stage::new();
        let (handles, animations) = circles(&mut stage);
        let mut group = broadcast(&mut stage, animations, 3.0, 0.2, remover).unwrap();
        group.begin(&mut stage).unwrap();
        group.finish(&mut stage);
        group.clean_up_from_scene(&mut stage);
        for mob in handles {
            assert_eq!(stage.roots().contains(&mob), !remover);
            assert!(!stage.is_animating(mob));
        }
    }
}
