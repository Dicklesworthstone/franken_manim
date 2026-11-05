//! fm-3kr tranche 1: `ClockPassesTime` wiring — the two Rotating members
//! about the clock's center, the Reference's defaults, and the documented
//! hour/minute angle ratio.

use fmn_anim::animation::Animation;
use fmn_anim::{AnimError, clock_passes_time, rotate_default};
use fmn_mobject::{Mob, Mobject, Stage, StageError};

fn point_mob(stage: &mut Stage, point: [f64; 3]) -> Mob {
    stage.add(Mobject::from_points(&[point]))
}

fn clock_family(stage: &mut Stage) -> [Mob; 3] {
    let clock = stage.add(Mobject::group(vec![
        Mobject::from_points(&[[-1.0, -1.0, 0.0], [1.0, 1.0, 0.0]]),
        Mobject::from_points(&[[0.0, 0.0, 0.0], [0.0, 0.3, 0.0]]),
        Mobject::from_points(&[[0.0, 0.0, 0.0], [0.0, 0.6, 0.0]]),
        Mobject::new(),
    ]));
    let children = stage.get(clock).unwrap().submobjects();
    [clock, children[1], children[2]]
}

#[test]
fn clock_passes_time_wires_two_rotations_with_reference_defaults() {
    let mut stage = Stage::new();
    let [clock, hour_hand, minute_hand] = clock_family(&mut stage);
    stage.add_to_scene(clock).unwrap();
    let snapshot = stage.snapshot_bytes().unwrap();

    let group = clock_passes_time(&mut stage, clock, hour_hand, minute_hand, 5.0, 12.0)
        .expect("wiring succeeds");

    assert_eq!(group.state().config.name, "ClockPassesTime");
    assert_eq!(group.state().config.run_time, 5.0);
    assert!(!group.state().config.remover);
    assert_eq!(group.animations().len(), 2, "hour + minute rotations");
    assert_eq!(group.state().mobject(), clock, "Reference group=clock");
    assert_eq!(stage.get(hour_hand).unwrap().parents(), &[clock]);
    assert_eq!(stage.get(minute_hand).unwrap().parents(), &[clock]);
    assert_eq!(stage.snapshot_bytes().unwrap(), snapshot);
}

#[test]
fn clock_passes_time_refuses_invalid_handles_without_mutating_the_stage() {
    for invalid_slot in 0..3 {
        for foreign in [false, true] {
            let mut stage = Stage::new();
            let mut other_stage = Stage::new();
            let mut handles = clock_family(&mut stage);
            stage.add_to_scene(handles[0]).unwrap();
            let invalid = if foreign {
                other_stage.add(Mobject::new())
            } else {
                let removed = stage.add(Mobject::new());
                stage.delete(removed).unwrap();
                removed
            };
            handles[invalid_slot] = invalid;
            let snapshot = stage.snapshot_bytes().unwrap();
            let [clock, hour_hand, minute_hand] = handles;
            let result = clock_passes_time(&mut stage, clock, hour_hand, minute_hand, 5.0, 12.0);
            let expected = if invalid_slot == 0 {
                AnimError::StaleHandle(invalid)
            } else {
                AnimError::Stage(StageError::StaleHandle)
            };
            assert_eq!(
                result.err(),
                Some(expected),
                "slot={invalid_slot}, foreign={foreign}"
            );
            assert_eq!(stage.snapshot_bytes().unwrap(), snapshot);
        }
    }
}

#[test]
fn rotate_default_still_stands_as_the_baseline() {
    // Guard against the shelf tranche disturbing the rotation surface.
    let mut stage = Stage::new();
    let mob = point_mob(&mut stage, [1.0, 0.0, 0.0]);
    let rotating = rotate_default(mob);
    assert_eq!(rotating.state().config.name, "Rotate");
}
