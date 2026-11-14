//! Released host callbacks obey the same authored lifecycle order as native plays.
use std::cell::RefCell;
use std::rc::Rc;

use fmn_anim::{AnimConfig, AnimError, AnimState, Animation, FramePacket, prepare_animation};
use fmn_mobject::animate::AnimateArgs;
use fmn_mobject::{Mob, Mobject, Stage};
use fmn_scene::{CaptureReason, IntegrationError, PlayOverrides, RuntimeConfig, Scene, SceneSink};

#[derive(Default)]
struct Sink(Vec<FramePacket>);

impl SceneSink for Sink {
    fn capture(&mut self, _: CaptureReason, frame: FramePacket) -> Result<(), IntegrationError> {
        self.0.push(frame);
        Ok(())
    }
}

type EventLog = Rc<RefCell<Vec<(&'static str, usize, f64, bool)>>>;

struct Probe {
    state: AnimState,
    shared: Mob,
    index: usize,
    native: bool,
    log: EventLog,
}

impl Probe {
    fn record(&self, stage: &Stage, phase: &'static str) {
        self.log.borrow_mut().push((
            phase,
            self.index,
            stage.tracker_value(self.shared).unwrap(),
            stage.roots().contains(&self.state.mobject()),
        ));
    }
}

impl Animation for Probe {
    fn state(&self) -> &AnimState {
        &self.state
    }

    fn state_mut(&mut self) -> &mut AnimState {
        &mut self.state
    }

    fn begin(&mut self, stage: &mut Stage) -> Result<(), AnimError> {
        if self.native {
            self.record(stage, "begin");
            let value = stage.tracker_value(self.shared).unwrap();
            stage.set_tracker_value(self.shared, value + 1.0).unwrap();
        }
        Ok(())
    }

    fn interpolate_submobject(&mut self, _: &mut Stage, _: &[Mob], _: f64) {}

    fn finish(&mut self, stage: &mut Stage) {
        if self.native {
            self.record(stage, "finish");
            let value = stage.tracker_value(self.shared).unwrap();
            stage.set_tracker_value(self.shared, value + 10.0).unwrap();
        }
    }

    fn clean_up_from_scene(&mut self, stage: &mut Stage) {
        if self.native {
            self.record(stage, "cleanup");
            stage.remove_from_scene(self.state.mobject());
        }
    }

    fn abort(&mut self, stage: &mut Stage) {
        self.record(stage, "abort");
    }
}

fn scene(skip: bool) -> Scene {
    Scene::new(
        RuntimeConfig {
            fps: 4,
            skip_animations: skip,
            preview_while_skipping: false,
            ..RuntimeConfig::default()
        },
        42,
    )
    .unwrap()
}

fn probe(mob: Mob, shared: Mob, index: usize, native: bool, log: &EventLog) -> Box<dyn Animation> {
    Box::new(Probe {
        state: AnimState::new(
            mob,
            AnimConfig {
                run_time: 0.5,
                suspend_mobject_updating: false,
                ..AnimConfig::default()
            },
        ),
        shared,
        index,
        native,
        log: Rc::clone(log),
    })
}

#[test]
fn mixed_begins_and_finishes_observe_authored_order_including_skip_mode() {
    for skip in [false, true] {
        let mut scene = scene(skip);
        let shared = scene.stage_mut().add_value_tracker(0.0);
        let mobs = std::array::from_fn::<_, 3, _>(|_| scene.stage_mut().add(Mobject::new()));
        let log = Rc::new(RefCell::new(Vec::new()));
        let animations = mobs
            .iter()
            .enumerate()
            .map(|(index, &mob)| probe(mob, shared, index, index != 1, &log))
            .collect();
        let mut sink = Sink::default();
        let mut setup = scene
            .prepare_stepped_play(animations, PlayOverrides::default(), &mut sink)
            .unwrap()
            .unwrap();
        assert!(log.borrow().is_empty(), "preparation must not begin a member");
        assert!(scene.complete_stepped_play_begin(&mut setup).is_err());
        while let Some(index) = scene.begin_stepped_play_animation(&mut setup).unwrap() {
            assert!(!scene.stage().roots().contains(&mobs[index]));
            assert!(scene.begin_stepped_play_animation(&mut setup).is_err());
            if index == 1 {
                assert!(scene.stage().roots().contains(&mobs[0]));
                assert_eq!(scene.stage().tracker_value(shared), Some(1.0));
                log.borrow_mut().push(("host begin", 1, 1.0, false));
                scene.stage_mut().set_tracker_value(shared, 7.0).unwrap();
            }
            scene.complete_stepped_play_begin(&mut setup).unwrap();
        }
        let mut play = scene.complete_stepped_play_setup(setup).unwrap();
        assert_eq!(scene.stage().roots(), &mobs);
        while scene.prepare_stepped_play_frame(&mut play).unwrap().is_some() {
            scene
                .complete_stepped_play_frame(&mut play, &mut sink)
                .unwrap();
        }
        while let Some((index, resumed)) = scene.finish_stepped_play_animation(&mut play).unwrap() {
            assert!(resumed.is_empty());
            assert!(scene.stage().roots().contains(&mobs[index]));
            assert!(scene.finish_stepped_play_animation(&mut play).is_err());
            assert!(scene.prepare_stepped_play_frame(&mut play).is_err());
            if index == 1 {
                assert!(!scene.stage().roots().contains(&mobs[0]));
                assert_eq!(scene.stage().tracker_value(shared), Some(18.0));
                log.borrow_mut().push(("host finish", 1, 18.0, true));
                scene.stage_mut().set_tracker_value(shared, 30.0).unwrap();
                scene.stage_mut().remove_from_scene(mobs[1]);
            }
            scene.complete_stepped_play_finish(&mut play).unwrap();
        }
        assert!(scene.finish_stepped_play_animations(&mut play).unwrap().is_empty());
        scene.finish_stepped_play(play, &mut sink).unwrap();
        assert_eq!(
            *log.borrow(),
            [
                ("begin", 0, 0.0, false),
                ("host begin", 1, 1.0, false),
                ("begin", 2, 7.0, false),
                ("finish", 0, 8.0, true),
                ("cleanup", 0, 18.0, true),
                ("host finish", 1, 18.0, true),
                ("finish", 2, 30.0, true),
                ("cleanup", 2, 40.0, true),
            ]
        );
        assert_eq!(scene.time().frames(), 2);
        assert_eq!(scene.play_count(), 1);
        assert_eq!(sink.0.len(), if skip { 0 } else { 2 });
        assert!(scene.stage().roots().is_empty());
    }
}

fn shift(scene: &mut Scene, mob: Mob) -> Box<dyn Animation> {
    let builder = mob
        .animate()
        .set_anim_args(AnimateArgs {
            run_time: Some(0.5),
            ..AnimateArgs::default()
        })
        .unwrap()
        .shift([2.0, 0.0, 0.0])
        .unwrap();
    prepare_animation(builder, scene.stage_mut()).unwrap()
}

#[test]
fn resumed_host_updaters_run_before_native_remover_cleanup() {
    let mut scene = scene(false);
    let mob = scene.add_mobject(Mobject::from_points(&[[0.0; 3]])).unwrap();
    let native_ticks = Rc::new(RefCell::new(Vec::new()));
    let ticks = Rc::clone(&native_ticks);
    scene
        .stage_mut()
        .add_dt_updater(
            mob,
            move |stage, target, dt| {
                if target == mob {
                    ticks.borrow_mut().push((dt, stage.roots().contains(&target)));
                }
            },
            false,
        )
        .unwrap();
    let mut animation = shift(&mut scene, mob);
    animation.state_mut().config.remover = true;
    animation.state_mut().config.suspend_mobject_updating = true;
    let mut sink = Sink::default();
    let mut play = scene
        .begin_stepped_play(vec![animation], PlayOverrides::default(), &mut sink)
        .unwrap()
        .unwrap();
    while scene.prepare_stepped_play_frame(&mut play).unwrap().is_some() {
        scene
            .complete_stepped_play_frame(&mut play, &mut sink)
            .unwrap();
    }
    assert!(native_ticks.borrow().is_empty());
    let (index, resumed) = scene
        .finish_stepped_play_animation(&mut play)
        .unwrap()
        .unwrap();
    assert_eq!(index, 0);
    assert_eq!(resumed, [mob]);
    assert_eq!(*native_ticks.borrow(), [(0.0, true)]);
    assert!(!scene.stage().is_updating_suspended(mob));
    assert!(
        scene.stage().roots().contains(&mob),
        "host updater still sees scene membership"
    );
    scene.stage_mut().shift(mob, [1.0, 0.0, 0.0]); // resumed host updater
    scene.complete_stepped_play_finish(&mut play).unwrap();
    assert!(!scene.stage().roots().contains(&mob));
    assert!(scene.finish_stepped_play_animation(&mut play).unwrap().is_none());
    scene.finish_stepped_play(play, &mut sink).unwrap();
    assert_eq!(scene.stage().get_center(mob)[0], 3.0);
    assert_eq!(
        *native_ticks.borrow(),
        [(0.0, true)],
        "removed updater never reruns"
    );
}

#[test]
fn interrupted_setup_restores_native_arena_without_finishing_or_counting_a_play() {
    for invalid_completion in [false, true] {
        let mut scene = scene(false);
        let mob = scene.stage_mut().add(Mobject::from_points(&[[0.0; 3]]));
        let mut animation = shift(&mut scene, mob);
        animation.state_mut().config.suspend_mobject_updating = true;
        let before = scene.stage().snapshot_bytes().unwrap();
        let mut sink = Sink::default();
        let mut setup = scene
            .prepare_stepped_play(vec![animation], PlayOverrides::default(), &mut sink)
            .unwrap()
            .unwrap();
        assert_eq!(
            scene.begin_stepped_play_animation(&mut setup).unwrap(),
            Some(0)
        );
        assert!(scene.stage().is_animating(mob));
        assert!(scene.stage().is_updating_suspended(mob));
        scene.stage_mut().shift(mob, [9.0, 0.0, 0.0]); // failing host begin
        let transient = scene.stage_mut().add(Mobject::new());
        scene.stage_mut().add_to_scene(transient).unwrap();
        if invalid_completion {
            assert!(scene.complete_stepped_play_setup(setup).is_err());
        } else {
            scene.abort_stepped_play_setup(setup, &mut sink);
        }
        assert_eq!(scene.stage().snapshot_bytes().unwrap(), before);
        assert!(!scene.stage().is_animating(mob));
        assert!(!scene.stage().is_updating_suspended(mob));
        assert_eq!(scene.time().frames(), 0);
        assert_eq!(scene.play_count(), 0);
        assert!(sink.0.is_empty());
    }
}

#[test]
fn finish_callback_failure_aborts_only_unfinished_members_without_remover_cleanup() {
    let mut scene = scene(false);
    let shared = scene.stage_mut().add_value_tracker(0.0);
    let first = scene.stage_mut().add(Mobject::new());
    let second = scene.stage_mut().add(Mobject::new());
    let log = Rc::new(RefCell::new(Vec::new()));
    let mut sink = Sink::default();
    let mut play = scene
        .begin_stepped_play(
            vec![
                probe(first, shared, 0, true, &log),
                probe(second, shared, 1, true, &log),
            ],
            PlayOverrides::default(),
            &mut sink,
        )
        .unwrap()
        .unwrap();
    assert_eq!(
        scene.finish_stepped_play_animation(&mut play).unwrap().unwrap().0,
        0
    );
    scene.abort_stepped_play(play, &mut sink); // the released host finish raised
    assert_eq!(
        *log.borrow(),
        [
            ("begin", 0, 0.0, false),
            ("begin", 1, 1.0, false),
            ("finish", 0, 2.0, true),
            ("abort", 1, 12.0, true),
        ]
    );
    assert_eq!(scene.stage().roots(), &[first, second]);
    assert_eq!(scene.time().frames(), 0);
    assert!(sink.0.is_empty());
}
