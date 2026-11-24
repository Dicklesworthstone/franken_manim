//! The render handoff retains observed geometry/time without rerunning callbacks.

use std::cell::Cell;
use std::rc::Rc;

use fmn_anim::{FramePacket, RationalFrameClock};
use fmn_core::rng::RngRoot;
use fmn_mobject::{Mobject, Stage};

#[test]
fn unshared_and_journal_shared_packets_have_identical_frozen_render_state() {
    for shared in [false, true] {
        let mut stage = Stage::new();
        let mob = stage.add(Mobject::from_points(&[[1.0, 2.0, 0.0]]));
        stage.add_to_scene(mob).unwrap();
        let calls = Rc::new(Cell::new(0));
        let callback_calls = Rc::clone(&calls);
        stage
            .add_updater(
                mob,
                move |_, _| callback_calls.set(callback_calls.get() + 1),
                false,
            )
            .unwrap();
        let mut clock = RationalFrameClock::new(30).unwrap();
        clock.advance_frames(17).unwrap();
        let packet = FramePacket::freeze_barrier(&stage, &clock, &RngRoot::from_seed(7));
        let journal = shared.then(|| packet.clone());
        stage
            .get_mut(mob)
            .unwrap()
            .buffer
            .write(0, "point", &[99.0, 88.0, 0.0]);
        let frozen = packet.into_render_state();
        std::thread::spawn(move || {
            let mut frame = frozen.materialize();
            assert_eq!(frame.time(), 17.0 / 30.0);
            assert_eq!(
                frame.get(mob).unwrap().buffer.read(0, "point").unwrap(),
                [1.0, 2.0, 0.0]
            );
            assert!(frame.updater_ids(mob).is_empty());
            frame.update(0.5);
        })
        .join()
        .unwrap();
        assert_eq!(
            calls.get(),
            0,
            "rendering cannot call the updater a second time"
        );
        if let Some(journal) = journal {
            let mut replay = journal.materialize_stage();
            assert_eq!(replay.updater_ids(mob).len(), 1);
            replay.update(0.25);
            assert_eq!(
                calls.get(),
                1,
                "render projection must not strip the journal's callbacks"
            );
        }
    }
}

#[test]
fn visible_packet_projection_omits_detached_history_but_preserves_the_journal() {
    for shared in [false, true] {
        let mut stage = Stage::new();
        let visible = stage.add(Mobject::from_points(&[[1.0, 2.0, 0.0]]));
        stage.add_to_scene(visible).unwrap();
        let target = stage.generate_target(visible).unwrap();
        let detached = stage.add(Mobject::from_points(&[[3.0, 4.0, 0.0]]));
        stage.add_updater(detached, |_, _| {}, false).unwrap();
        let mut clock = RationalFrameClock::new(30).unwrap();
        clock.advance_frames(17).unwrap();
        let packet = FramePacket::freeze_barrier(&stage, &clock, &RngRoot::from_seed(7));
        let journal = shared.then(|| packet.clone());
        stage.shift(visible, [5.0, 0.0, 0.0]);
        let frozen = packet.into_visible_render_state();
        assert_eq!(frozen.retained_object_count(), 1);
        std::thread::spawn(move || {
            let frame = frozen.materialize();
            assert_eq!(frame.time(), 17.0 / 30.0);
            assert_eq!(frame.roots(), &[visible]);
            assert_eq!(frame.get_points(visible).unwrap(), [[1.0, 2.0, 0.0]]);
            assert!(!frame.contains(detached));
            assert!(!frame.contains(target));
        })
        .join()
        .unwrap();
        assert!(stage.contains(detached));
        assert!(stage.contains(target));
        if let Some(journal) = journal {
            let replay = journal.materialize_stage();
            assert!(replay.contains(target));
            assert_eq!(replay.target(visible), Some(target));
            assert_eq!(replay.updater_ids(detached).len(), 1);
        }
    }
}

#[test]
fn packet_auxiliary_dependencies_remain_frozen_and_unrooted() {
    for shared in [false, true] {
        let mut stage = Stage::new();
        let visible = stage.add(Mobject::from_points(&[[1.0, 2.0, 0.0]]));
        let light = stage.add(Mobject::from_points(&[[3.0, 4.0, 5.0]]));
        stage.add_to_scene(visible).unwrap();
        let clock = RationalFrameClock::new(30).unwrap();
        let packet = FramePacket::freeze_barrier(&stage, &clock, &RngRoot::from_seed(7));
        let journal = shared.then(|| packet.clone());
        stage.shift(light, [10.0, 0.0, 0.0]);
        let state = packet.into_visible_render_state_with_auxiliary(&[light]);
        assert_eq!(state.retained_object_count(), 2);
        let frame = state.materialize();
        assert_eq!(frame.roots(), &[visible]);
        assert_eq!(frame.get_center(light), [3.0, 4.0, 5.0]);
        if let Some(journal) = journal {
            assert_eq!(
                journal.materialize_stage().get_center(light),
                [3.0, 4.0, 5.0]
            );
        }
        assert_eq!(stage.get_center(light), [13.0, 4.0, 5.0]);
    }
}
