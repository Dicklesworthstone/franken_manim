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
