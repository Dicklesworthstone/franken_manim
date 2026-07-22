//! `.animate` builder fixture corpus (fm-yra acceptance): chained-call
//! recording against a build-time target, once-only animation arguments,
//! the override no-chaining rule in both directions, dynamic target lookup,
//! and the prepare_animation contract.

use fmn_core::constants::{ORIGIN, RIGHT, UP};
use fmn_mobject::animate::{AnimateArgs, AnimateError, OverrideAnimation};
use fmn_mobject::{IntoAnimate, Mobject, Stage};

fn square(stage: &mut Stage) -> fmn_mobject::Mob {
    stage.add(Mobject::from_points(&[
        [-0.5, -0.5, 0.0],
        [0.5, -0.5, 0.0],
        [0.5, 0.5, 0.0],
        [-0.5, 0.5, 0.0],
    ]))
}

#[test]
fn recording_applies_to_the_target_copy_not_the_source() {
    let mut stage = Stage::new();
    let mob = square(&mut stage);
    let before = stage.get_center(mob);

    let built = mob
        .animate()
        .shift([2.0, 0.0, 0.0])
        .and_then(|b| b.scale(2.0))
        .and_then(|b| b.build(&mut stage))
        .expect("chain builds");

    // Source untouched; target carries the composed result.
    assert_eq!(stage.get_center(mob), before);
    let target_center = stage.get_center(built.target);
    assert!((target_center[0] - 2.0).abs() < 1e-6);
    assert!((stage.get_width(built.target) - 2.0).abs() < 1e-6);
    assert_eq!(built.source, mob);
    assert_ne!(built.target, mob);
}

#[test]
fn anim_args_are_set_once_per_chain() {
    let mut stage = Stage::new();
    let mob = square(&mut stage);

    let args = AnimateArgs {
        run_time: Some(2.0),
        lag_ratio: Some(0.1),
        ..AnimateArgs::default()
    };
    let builder = mob
        .animate()
        .set_anim_args(args)
        .expect("first args pass is fine");
    // The second pass is the Reference's ValueError.
    assert_eq!(
        builder.clone().set_anim_args(AnimateArgs::default()),
        Err(AnimateError::ArgsAlreadySet)
    );
    let built = builder
        .shift([1.0, 0.0, 0.0])
        .and_then(|b| b.build(&mut stage))
        .expect("builds");
    assert_eq!(built.args.run_time, Some(2.0));
    assert_eq!(built.args.lag_ratio, Some(0.1));
}

#[test]
fn overridden_animations_do_not_chain_in_either_direction() {
    let mut stage = Stage::new();
    let mob = square(&mut stage);
    let ov = OverrideAnimation { name: "fade_stub" };

    // Override after a chained call: refused.
    assert_eq!(
        mob.animate()
            .shift([1.0, 0.0, 0.0])
            .and_then(|b| b.with_override(ov)),
        Err(AnimateError::OverrideNotChainable)
    );
    // Chained call after an override: refused.
    assert_eq!(
        mob.animate()
            .with_override(ov)
            .and_then(|b| b.shift([1.0, 0.0, 0.0])),
        Err(AnimateError::OverrideNotChainable)
    );
    // A second override: refused.
    assert_eq!(
        mob.animate()
            .with_override(ov)
            .and_then(|b| b.with_override(ov)),
        Err(AnimateError::OverrideNotChainable)
    );
    // An override alone builds, carrying its marker.
    let built = mob
        .animate()
        .with_override(ov)
        .and_then(|b| b.build(&mut stage))
        .expect("override alone builds");
    assert_eq!(built.overridden, Some(ov));
}

#[test]
fn mob_targets_resolve_at_build_time() {
    let mut stage = Stage::new();
    let mob = square(&mut stage);
    let anchor = square(&mut stage);

    // Record a next_to against the anchor, THEN move the anchor: the build
    // must see the anchor's position at build time (dynamic target lookup),
    // not where it was when the chain was written.
    let builder = mob
        .animate()
        .next_to(anchor, RIGHT, 0.25, ORIGIN)
        .expect("records");
    stage.shift(anchor, [5.0, 3.0, 0.0]);
    let built = builder.build(&mut stage).expect("builds");

    let anchor_right = stage.get_right(anchor);
    let target_left = stage.get_left(built.target);
    assert!((target_left[0] - (anchor_right[0] + 0.25)).abs() < 1e-6);
    assert!((stage.get_center(built.target)[1] - stage.get_center(anchor)[1]).abs() < 1e-6);
}

#[test]
fn stale_handles_fail_the_build_by_name() {
    let mut stage = Stage::new();
    let mob = square(&mut stage);
    let other = square(&mut stage);

    // Dead source.
    let doomed = square(&mut stage);
    let chain = doomed.animate().shift([1.0, 0.0, 0.0]).expect("records");
    stage.delete(doomed).unwrap();
    assert_eq!(
        chain.build(&mut stage),
        Err(AnimateError::StaleHandle(doomed))
    );

    // Dead Mob target inside a command — named precisely.
    let chain = mob.animate().move_to(other, UP).expect("records");
    stage.delete(other).unwrap();
    assert_eq!(
        chain.build(&mut stage),
        Err(AnimateError::StaleHandle(other))
    );
}

#[test]
fn prepare_animation_contract_accepts_builders_and_built() {
    let mut stage = Stage::new();
    let mob = square(&mut stage);

    // A builder prepares by building.
    let built = mob
        .animate()
        .shift([1.0, 1.0, 0.0])
        .expect("records")
        .prepare(&mut stage)
        .expect("builder prepares");
    // A built animation prepares as identity.
    let target = built.target;
    let again = built.prepare(&mut stage).expect("identity prepare");
    assert_eq!(again.target, target);
    // (A bare method is unrepresentable: only these two types implement
    // IntoAnimate — the typed form of the Reference's TypeError.)
}
