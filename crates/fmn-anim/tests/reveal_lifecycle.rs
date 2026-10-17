//! Every child must be reachable, and hidden children retain no animation lifetime.
use fmn_anim::{
    AnimConfig, Animation, RateFunc, ShowIncreasingSubsets, show_increasing_subsets,
    show_submobjects_one_by_one,
};
use fmn_mobject::{Mob, Mobject, Stage};
use std::{cell::RefCell, rc::Rc};

fn group(stage: &mut Stage, n: usize) -> (Mob, Vec<Mob>) {
    let root = stage.add(Mobject::new());
    let children = (0..n)
        .map(|i| {
            let child = stage.add(Mobject::from_points(&[[i as f64, 0.0, 0.0]]));
            let grandchild = stage.add(Mobject::from_points(&[[i as f64, 0.1, 0.0]]));
            stage.attach(child, grandchild).unwrap();
            stage.attach(root, child).unwrap();
            child
        })
        .collect();
    stage.add_to_scene(root).unwrap();
    (root, children)
}

fn config() -> AnimConfig {
    AnimConfig {
        rate_func: RateFunc::linear(),
        suspend_mobject_updating: true,
        ..AnimConfig::default()
    }
}

#[test]
fn one_by_one_reaches_each_child_including_the_only_or_last_child() {
    for n in [1, 2, 3, 7] {
        let mut stage = Stage::new();
        let (root, children) = group(&mut stage, n);
        let mut animation = show_submobjects_one_by_one(&stage, root)
            .unwrap()
            .with_config(config());
        animation.begin(&mut stage).unwrap();
        assert!(stage.get(root).unwrap().submobjects().is_empty());
        for (index, child) in children.iter().enumerate() {
            animation.interpolate(&mut stage, (index as f64 + 0.5) / n as f64);
            assert_eq!(stage.get(root).unwrap().submobjects(), &[*child]);
        }
        animation.finish(&mut stage);
        assert_eq!(stage.get(root).unwrap().submobjects(), &children[n - 1..]);
        animation.interpolate(&mut stage, -1.0);
        assert!(stage.get(root).unwrap().submobjects().is_empty());
        animation.interpolate(&mut stage, 2.0);
        assert_eq!(stage.get(root).unwrap().submobjects(), &children[n - 1..]);
    }
}

#[test]
fn hidden_members_release_animation_and_suspension_on_finish_or_abort() {
    for abort in [false, true] {
        for suspend in [false, true] {
            for final_alpha in [0.0, 0.4, 1.0] {
                let mut stage = Stage::new();
                let (root, children) = group(&mut stage, 5);
                let begun = stage.family(root);
                let mut animation =
                    show_increasing_subsets(&stage, root)
                        .unwrap()
                        .with_config(AnimConfig {
                            final_alpha_value: final_alpha,
                            suspend_mobject_updating: suspend,
                            ..config()
                        });
                animation.begin(&mut stage).unwrap();
                animation.interpolate(&mut stage, 0.4);
                let before = stage.get(root).unwrap().submobjects().to_vec();
                if abort {
                    animation.abort(&mut stage);
                } else {
                    animation.finish(&mut stage);
                }
                if abort {
                    assert_eq!(stage.get(root).unwrap().submobjects(), before);
                }
                for mob in begun {
                    assert!(
                        !stage.is_animating(mob),
                        "hidden member still animating: {mob:?}"
                    );
                    assert!(
                        !stage.is_updating_suspended(mob),
                        "hidden member still suspended: {mob:?}"
                    );
                }
                let calls = Rc::new(RefCell::new(Vec::new()));
                for child in children {
                    stage.add_to_scene(child).unwrap();
                    let calls = Rc::clone(&calls);
                    stage
                        .add_dt_updater(
                            child,
                            move |_, m, dt| calls.borrow_mut().push((m, dt)),
                            false,
                        )
                        .unwrap();
                }
                stage.update(0.25);
                assert!(!calls.borrow().is_empty());
            }
        }
    }
}

#[test]
fn cleanup_does_not_execute_hidden_updaters_or_repeat_visible_ones() {
    for abort in [false, true] {
        let mut stage = Stage::new();
        let (root, children) = group(&mut stage, 3);
        let seen = Rc::new(RefCell::new(Vec::new()));
        for &child in &children {
            let seen = Rc::clone(&seen);
            stage
                .add_dt_updater(
                    child,
                    move |_, mob, dt| seen.borrow_mut().push((mob, dt)),
                    false,
                )
                .unwrap();
        }
        let mut animation =
            show_increasing_subsets(&stage, root)
                .unwrap()
                .with_config(AnimConfig {
                    final_alpha_value: 0.4,
                    ..config()
                });
        animation.begin(&mut stage).unwrap();
        animation.interpolate(&mut stage, 0.4);
        seen.borrow_mut().clear();
        if abort {
            animation.abort(&mut stage);
        } else {
            animation.finish(&mut stage);
        }
        if abort {
            assert!(seen.borrow().is_empty());
        } else {
            assert_eq!(*seen.borrow(), vec![(children[0], 0.0)]);
        }
        for &child in &children {
            assert!(!stage.is_updating_suspended(child));
        }
    }
}

#[test]
fn an_already_suspended_reveal_is_not_resumed_by_cleanup() {
    let mut stage = Stage::new();
    let (root, _) = group(&mut stage, 3);
    let begun = stage.family(root);
    stage.suspend_updating(root, true);
    let mut animation: ShowIncreasingSubsets = show_increasing_subsets(&stage, root)
        .unwrap()
        .with_config(AnimConfig {
            final_alpha_value: 0.0,
            ..config()
        });
    animation.begin(&mut stage).unwrap();
    animation.finish(&mut stage);
    for member in begun {
        assert!(stage.is_updating_suspended(member));
        assert!(!stage.is_animating(member));
    }
}
