//! fm-eiw acceptance: dispatcher laws, Reference keyboard fixture,
//! InteractiveScene behavior, journal replay, and the serial pre-capture seam.

use std::cell::RefCell;
use std::rc::Rc;

use fmn_anim::{FramePacket, RationalTime};
use fmn_core::color::Srgb;
use fmn_mobject::{Mob, Mobject, RecordBuffer, RecordSchema, Stage};
use fmn_scene::{
    CaptureReason, EventDispatcher, EventError, EventListener, EventPayload, EventPropagation,
    EventTarget, EventType, HoldDecision, HoldKind, InputEvent, IntegrationError,
    InteractiveAction, InteractiveClipboard, InteractiveScene, Journal, Key, Modifiers,
    MouseButton, REFERENCE_KEYBOARD_MAP, RuntimeConfig, Scene, SceneSink,
};

fn input(sequence: u64, payload: EventPayload) -> InputEvent {
    InputEvent::new(sequence, RationalTime::zero(30), payload).expect("valid fixture event")
}

fn rectangle(center: [f64; 3], color: Srgb) -> Mobject {
    let points = [
        [center[0] - 0.5, center[1] - 0.5, center[2]],
        [center[0] + 0.5, center[1] - 0.5, center[2]],
        [center[0] + 0.5, center[1] + 0.5, center[2]],
        [center[0] - 0.5, center[1] + 0.5, center[2]],
    ];
    let mut buffer = RecordBuffer::new(RecordSchema::vmobject(), points.len());
    #[allow(clippy::cast_possible_truncation)]
    let flat_points: Vec<f32> = points
        .iter()
        .flat_map(|point| point.iter().map(|value| *value as f32))
        .collect();
    buffer.write_range("point", 0, &flat_points);
    #[allow(clippy::cast_possible_truncation)]
    let rgba = [color.r as f32, color.g as f32, color.b as f32, 1.0];
    buffer.write_range("fill_rgba", 0, &rgba.repeat(points.len()));
    buffer.write_range("stroke_rgba", 0, &rgba.repeat(points.len()));
    buffer.write_range("stroke_width", 0, &vec![1.0; points.len()]);
    Mobject::from_buffer(buffer)
}

fn fill_color(stage: &Stage, mob: Mob) -> Srgb {
    let rgba = stage
        .get(mob)
        .and_then(|entry| entry.buffer.read(0, "fill_rgba"))
        .expect("vmobject fill");
    Srgb {
        r: f64::from(rgba[0]),
        g: f64::from(rgba[1]),
        b: f64::from(rgba[2]),
    }
}

#[test]
fn dispatcher_preserves_order_hit_testing_removal_and_explicit_stop() {
    let mut stage = Stage::new();
    let target = stage.add(rectangle([0.0; 3], Srgb::from_hex("#FFFFFF").unwrap()));
    stage.add_to_scene(target).unwrap();

    let calls = Rc::new(RefCell::new(Vec::new()));
    let mut dispatcher = EventDispatcher::new();
    let first_calls = Rc::clone(&calls);
    let first = dispatcher
        .add_listener(EventListener::new(
            EventType::MousePress,
            EventTarget::Mobject(target),
            move |_, _, _, _| {
                first_calls.borrow_mut().push("first");
                EventPropagation::Continue
            },
        ))
        .unwrap();
    let stop_calls = Rc::clone(&calls);
    let stop = dispatcher
        .add_listener(EventListener::new(
            EventType::MousePress,
            EventTarget::Global,
            move |_, _, _, _| {
                stop_calls.borrow_mut().push("stop");
                EventPropagation::Stop
            },
        ))
        .unwrap();
    let last_calls = Rc::clone(&calls);
    dispatcher
        .add_listener(EventListener::new(
            EventType::MousePress,
            EventTarget::Global,
            move |_, _, _, _| {
                last_calls.borrow_mut().push("last");
                EventPropagation::Continue
            },
        ))
        .unwrap();

    let press = input(
        0,
        EventPayload::MousePress {
            point: [0.0; 3],
            button: MouseButton::Left,
            modifiers: Modifiers::NONE,
        },
    );
    assert_eq!(
        dispatcher.dispatch(&press, &mut stage),
        EventPropagation::Stop
    );
    assert_eq!(&*calls.borrow(), &["first", "stop"]);
    assert_eq!(
        dispatcher.listener_ids(EventType::MousePress)[..2],
        [first, stop]
    );

    assert!(dispatcher.remove_listener(stop));
    assert!(!dispatcher.remove_listener(stop), "removal is idempotent");
    calls.borrow_mut().clear();
    dispatcher.dispatch(&press, &mut stage);
    assert_eq!(&*calls.borrow(), &["first", "last"]);

    calls.borrow_mut().clear();
    let miss = input(
        1,
        EventPayload::MousePress {
            point: [9.0, 9.0, 0.0],
            button: MouseButton::Left,
            modifiers: Modifiers::NONE,
        },
    );
    dispatcher.dispatch(&miss, &mut stage);
    assert_eq!(
        &*calls.borrow(),
        &["last"],
        "mobject listener is hit-tested; global listener remains eligible"
    );
}

#[test]
fn dispatcher_captures_drag_targets_and_tracks_pressed_keys() {
    let mut stage = Stage::new();
    let target = stage.add(rectangle([0.0; 3], Srgb::from_hex("#FFFFFF").unwrap()));
    stage.add_to_scene(target).unwrap();
    let drags = Rc::new(RefCell::new(Vec::new()));
    let seen = Rc::clone(&drags);
    let mut dispatcher = EventDispatcher::new();
    dispatcher
        .add_listener(EventListener::new(
            EventType::MouseDrag,
            EventTarget::Mobject(target),
            move |event, _, state, _| {
                seen.borrow_mut()
                    .push((event.sequence, state.mouse_drag_point()));
                EventPropagation::Continue
            },
        ))
        .unwrap();

    dispatcher.dispatch(
        &input(
            0,
            EventPayload::MousePress {
                point: [0.0; 3],
                button: MouseButton::Left,
                modifiers: Modifiers::NONE,
            },
        ),
        &mut stage,
    );
    dispatcher.dispatch(
        &input(
            1,
            EventPayload::MouseDrag {
                point: [5.0, 0.0, 0.0],
                delta: [5.0, 0.0, 0.0],
                button: MouseButton::Left,
                modifiers: Modifiers::NONE,
            },
        ),
        &mut stage,
    );
    assert_eq!(&*drags.borrow(), &[(1, [5.0, 0.0, 0.0])]);

    dispatcher.dispatch(
        &input(
            2,
            EventPayload::MouseRelease {
                point: [5.0, 0.0, 0.0],
                button: MouseButton::Left,
                modifiers: Modifiers::NONE,
            },
        ),
        &mut stage,
    );
    dispatcher.dispatch(
        &input(
            3,
            EventPayload::MouseDrag {
                point: [0.0; 3],
                delta: [-5.0, 0.0, 0.0],
                button: MouseButton::Left,
                modifiers: Modifiers::NONE,
            },
        ),
        &mut stage,
    );
    assert_eq!(drags.borrow().len(), 1, "release clears drag capture");

    dispatcher.dispatch(
        &input(
            4,
            EventPayload::KeyPress {
                key: Key::Character('G'),
                modifiers: Modifiers::SHIFT,
            },
        ),
        &mut stage,
    );
    dispatcher.dispatch(
        &input(
            5,
            EventPayload::KeyPress {
                key: Key::Character('g'),
                modifiers: Modifiers::SHIFT,
            },
        ),
        &mut stage,
    );
    assert_eq!(
        dispatcher.state().pressed_keys(),
        &[Key::Character('g')],
        "repeat presses do not duplicate and shortcut characters canonicalize"
    );
    dispatcher.dispatch(
        &input(
            6,
            EventPayload::KeyRelease {
                key: Key::Character('G'),
                modifiers: Modifiers::NONE,
            },
        ),
        &mut stage,
    );
    assert!(dispatcher.state().pressed_keys().is_empty());
}

#[test]
fn keyboard_fixture_matches_the_pinned_reference_defaults() {
    let actual: Vec<_> = REFERENCE_KEYBOARD_MAP
        .iter()
        .map(|binding| (binding.action, binding.key))
        .collect();
    assert_eq!(
        actual,
        [
            (InteractiveAction::Pan3D, Key::Character('d')),
            (InteractiveAction::Pan, Key::Character('f')),
            (InteractiveAction::Reset, Key::Character('r')),
            (InteractiveAction::Quit, Key::Character('q')),
            (InteractiveAction::Select, Key::Character('s')),
            (InteractiveAction::Unselect, Key::Character('u')),
            (InteractiveAction::Grab, Key::Character('g')),
            (InteractiveAction::XGrab, Key::Character('h')),
            (InteractiveAction::YGrab, Key::Character('v')),
            (InteractiveAction::ZGrab, Key::Character('z')),
            (InteractiveAction::Resize, Key::Character('t')),
            (InteractiveAction::Color, Key::Character('c')),
            (InteractiveAction::Information, Key::Character('i')),
            (InteractiveAction::Cursor, Key::Character('k')),
        ]
    );
}

#[test]
fn invalid_and_out_of_order_inputs_fail_before_scene_mutation() {
    let mut scene = Scene::default();
    assert_eq!(
        scene.queue_event(EventPayload::MouseMotion {
            point: [f64::NAN, 0.0, 0.0],
            delta: [0.0; 3],
            modifiers: Modifiers::NONE,
        }),
        Err(EventError::NonFiniteCoordinate)
    );
    assert!(scene.recorded_events().is_empty());

    let wrong_grid = InputEvent::new(
        0,
        RationalTime::zero(60),
        EventPayload::KeyPress {
            key: Key::Character('s'),
            modifiers: Modifiers::NONE,
        },
    )
    .expect("valid event on a different grid");
    assert_eq!(
        scene.queue_replay_events(&[wrong_grid]),
        Err(EventError::TimestampGridMismatch {
            expected: 30,
            found: 60
        })
    );

    let later = input(
        2,
        EventPayload::KeyPress {
            key: Key::Character('s'),
            modifiers: Modifiers::NONE,
        },
    );
    let earlier_sequence = input(
        1,
        EventPayload::KeyRelease {
            key: Key::Character('s'),
            modifiers: Modifiers::NONE,
        },
    );
    assert_eq!(
        scene.queue_replay_events(&[later, earlier_sequence]),
        Err(EventError::ReplayOutOfOrder)
    );

    let later_time = InputEvent::new(
        2,
        RationalTime::zero(30) + 1,
        EventPayload::KeyPress {
            key: Key::Character('s'),
            modifiers: Modifiers::NONE,
        },
    )
    .expect("later event");
    let lower_sequence_at_still_later_time = InputEvent::new(
        1,
        RationalTime::zero(30) + 2,
        EventPayload::KeyRelease {
            key: Key::Character('s'),
            modifiers: Modifiers::NONE,
        },
    )
    .expect("still-later event");
    assert_eq!(
        scene.queue_replay_events(&[later_time, lower_sequence_at_still_later_time]),
        Err(EventError::ReplayOutOfOrder),
        "sequence ids remain globally monotonic when time advances"
    );

    let mut fresh = Scene::default();
    fresh
        .queue_replay_events(&[InputEvent {
            sequence: 0,
            timestamp: RationalTime::zero(30),
            payload: EventPayload::KeyPress {
                key: Key::Character('S'),
                modifiers: Modifiers::NONE,
            },
        }])
        .expect("public event fields are recanonicalized at replay ingestion");
    assert_eq!(fresh.dispatch_pending_events().expect("dispatch"), 1);
    assert!(matches!(
        fresh.recorded_events()[0].payload,
        EventPayload::KeyPress {
            key: Key::Character('s'),
            ..
        }
    ));
}

fn scripted_scene() -> (InteractiveScene, Mob, Mob) {
    let mut scene = Scene::new(
        RuntimeConfig {
            fps: 30,
            ..RuntimeConfig::default()
        },
        17,
    )
    .unwrap();
    let selected = scene
        .add_mobject(rectangle(
            [-2.0, 0.0, 0.0],
            Srgb::from_hex("#FF0000").unwrap(),
        ))
        .unwrap();
    let color_source = scene
        .add_mobject(rectangle(
            [4.0, 0.0, 0.0],
            Srgb::from_hex("#0000FF").unwrap(),
        ))
        .unwrap();
    (
        InteractiveScene::new(scene).unwrap(),
        selected,
        color_source,
    )
}

fn scripted_payloads() -> Vec<EventPayload> {
    vec![
        EventPayload::MouseMotion {
            point: [-2.6, -0.6, 0.0],
            delta: [-2.6, -0.6, 0.0],
            modifiers: Modifiers::NONE,
        },
        EventPayload::KeyPress {
            key: Key::Character('s'),
            modifiers: Modifiers::NONE,
        },
        EventPayload::MouseMotion {
            point: [-1.4, 0.6, 0.0],
            delta: [1.2, 1.2, 0.0],
            modifiers: Modifiers::NONE,
        },
        EventPayload::KeyRelease {
            key: Key::Character('s'),
            modifiers: Modifiers::NONE,
        },
        EventPayload::MouseMotion {
            point: [-2.0, 0.0, 0.0],
            delta: [-0.6, -0.6, 0.0],
            modifiers: Modifiers::NONE,
        },
        EventPayload::KeyPress {
            key: Key::Character('g'),
            modifiers: Modifiers::NONE,
        },
        EventPayload::MouseMotion {
            point: [0.0, 1.0, 0.0],
            delta: [2.0, 1.0, 0.0],
            modifiers: Modifiers::NONE,
        },
        EventPayload::KeyRelease {
            key: Key::Character('g'),
            modifiers: Modifiers::NONE,
        },
        EventPayload::MouseMotion {
            point: [0.5, 1.0, 0.0],
            delta: [0.5, 0.0, 0.0],
            modifiers: Modifiers::NONE,
        },
        EventPayload::KeyPress {
            key: Key::Character('t'),
            modifiers: Modifiers::NONE,
        },
        EventPayload::MouseMotion {
            point: [1.0, 1.0, 0.0],
            delta: [0.5, 0.0, 0.0],
            modifiers: Modifiers::NONE,
        },
        EventPayload::KeyRelease {
            key: Key::Character('t'),
            modifiers: Modifiers::NONE,
        },
        EventPayload::KeyPress {
            key: Key::Character('c'),
            modifiers: Modifiers::NONE,
        },
        EventPayload::MouseRelease {
            point: [4.0, 0.0, 0.0],
            button: MouseButton::Left,
            modifiers: Modifiers::NONE,
        },
        EventPayload::KeyPress {
            key: Key::Character('c'),
            modifiers: Modifiers::CONTROL,
        },
        EventPayload::KeyPress {
            key: Key::Character('v'),
            modifiers: Modifiers::CONTROL,
        },
    ]
}

fn run_script(scene: &mut InteractiveScene) {
    for payload in scripted_payloads() {
        scene.queue_event(payload).unwrap();
    }
    assert_eq!(scene.dispatch_pending_events().unwrap(), 16);
}

#[test]
fn select_grab_resize_color_and_mobject_paste_have_scene_state_outcomes() {
    let (mut interactive, selected, color_source) = scripted_scene();
    run_script(&mut interactive);

    let selection = interactive.selection();
    assert_eq!(selection.len(), 1, "paste selects the new copy");
    let pasted = selection[0];
    assert_ne!(pasted, selected);
    assert_eq!(interactive.stage().roots().len(), 3);
    assert_eq!(interactive.stage().get_center(selected), [0.0, 1.0, 0.0]);
    assert!((interactive.stage().get_bounding_box(selected).width() - 2.0).abs() < 1.0e-9);
    assert_eq!(interactive.stage().get_center(pasted), [0.0, 1.0, 0.0]);
    assert_eq!(
        fill_color(interactive.stage(), selected),
        fill_color(interactive.stage(), color_source)
    );
    assert_eq!(
        fill_color(interactive.stage(), pasted),
        fill_color(interactive.stage(), color_source)
    );
    assert!(matches!(
        interactive.clipboard(),
        InteractiveClipboard::Mobjects(ref templates) if templates.len() == 1
    ));
    let highlights = interactive.selection_highlights();
    assert_eq!(highlights.len(), 1);
    assert_eq!(highlights[0].target, pasted);
}

#[test]
fn journaled_event_stream_replays_to_identical_scene_state() {
    let (mut original, _, _) = scripted_scene();
    run_script(&mut original);
    let mut journal = Journal::new();
    journal.record_events(original.recorded_events()).unwrap();
    let decoded = Journal::from_bytes(&journal.to_bytes().unwrap()).unwrap();
    assert_eq!(decoded.events(), original.recorded_events());

    let (mut replay, _, _) = scripted_scene();
    replay.queue_replay_events(decoded.events()).unwrap();
    assert_eq!(replay.dispatch_pending_events().unwrap(), 16);
    assert_eq!(
        original.stage().snapshot_bytes().unwrap(),
        replay.stage().snapshot_bytes().unwrap(),
        "recorded input reproduces the complete arena, including clipboard templates"
    );
    assert_eq!(original.selection().len(), replay.selection().len());
}

#[derive(Default)]
struct PacketSink {
    packets: Vec<(CaptureReason, FramePacket)>,
}

impl SceneSink for PacketSink {
    fn capture(
        &mut self,
        reason: CaptureReason,
        packet: FramePacket,
    ) -> Result<(), IntegrationError> {
        self.packets.push((reason, packet));
        Ok(())
    }
}

#[test]
fn scene_dispatches_after_updaters_and_before_immutable_capture() {
    let mut scene = Scene::new(
        RuntimeConfig {
            fps: 1,
            ..RuntimeConfig::default()
        },
        5,
    )
    .unwrap();
    let mob = scene
        .add_mobject(rectangle([0.0; 3], Srgb::from_hex("#FFFFFF").unwrap()))
        .unwrap();
    scene
        .stage_mut()
        .add_dt_updater(
            mob,
            |stage, me, dt| {
                if dt > 0.0 {
                    stage.shift(me, [1.0, 0.0, 0.0]);
                }
            },
            false,
        )
        .unwrap();
    let observed = Rc::new(RefCell::new(Vec::new()));
    let event_observed = Rc::clone(&observed);
    scene
        .event_dispatcher_mut()
        .add_listener(EventListener::new(
            EventType::KeyPress,
            EventTarget::Global,
            move |_, _, _, stage| {
                event_observed.borrow_mut().push(stage.get_center(mob)[0]);
                stage.shift(mob, [10.0, 0.0, 0.0]);
                EventPropagation::Continue
            },
        ))
        .unwrap();
    scene
        .queue_event(EventPayload::KeyPress {
            key: Key::Character('g'),
            modifiers: Modifiers::NONE,
        })
        .unwrap();

    let mut sink = PacketSink::default();
    scene.wait(Some(1.0), &mut sink).unwrap();
    assert_eq!(&*observed.borrow(), &[1.0]);
    assert_eq!(
        scene.recorded_events()[0].timestamp,
        RationalTime::zero(1) + 1
    );
    assert_eq!(sink.packets.len(), 1);
    assert_eq!(
        sink.packets[0].1.materialize_stage().get_center(mob)[0],
        11.0
    );
    scene.stage_mut().shift(mob, [100.0, 0.0, 0.0]);
    assert_eq!(
        sink.packets[0].1.materialize_stage().get_center(mob)[0],
        11.0,
        "later live mutation cannot enter the frozen packet"
    );
}

#[test]
fn cloneable_host_inbox_enters_the_same_serial_boundary() {
    let mut scene = Scene::new(
        RuntimeConfig {
            fps: 1,
            ..RuntimeConfig::default()
        },
        6,
    )
    .expect("scene");
    let mob = scene
        .add_mobject(rectangle(
            [0.0; 3],
            Srgb::from_hex("#FFFFFF").expect("fixture color"),
        ))
        .expect("mobject");
    scene
        .event_dispatcher_mut()
        .add_listener(EventListener::new(
            EventType::KeyPress,
            EventTarget::Global,
            move |_, _, _, stage| {
                stage.shift(mob, [3.0, 0.0, 0.0]);
                EventPropagation::Continue
            },
        ))
        .expect("listener");

    let inbox = scene.event_inbox();
    std::thread::spawn(move || {
        inbox
            .submit(EventPayload::KeyPress {
                key: Key::Character('g'),
                modifiers: Modifiers::NONE,
            })
            .expect("host payload");
    })
    .join()
    .expect("host thread");

    let mut sink = PacketSink::default();
    scene.wait(Some(1.0), &mut sink).expect("wait");
    assert_eq!(scene.recorded_events().len(), 1);
    assert_eq!(
        scene.recorded_events()[0].timestamp,
        RationalTime::zero(1) + 1
    );
    assert_eq!(
        sink.packets[0].1.materialize_stage().get_center(mob)[0],
        3.0
    );
}

#[test]
fn presenter_hold_drains_input_before_poll_and_capture() {
    let mut scene = Scene::new(
        RuntimeConfig {
            fps: 1,
            presenter_mode: true,
            ..RuntimeConfig::default()
        },
        9,
    )
    .unwrap();
    let mob = scene
        .add_mobject(rectangle([0.0; 3], Srgb::from_hex("#FFFFFF").unwrap()))
        .unwrap();
    scene
        .event_dispatcher_mut()
        .add_listener(EventListener::new(
            EventType::KeyPress,
            EventTarget::Global,
            move |_, _, _, stage| {
                stage.shift(mob, [5.0, 0.0, 0.0]);
                EventPropagation::Continue
            },
        ))
        .unwrap();
    scene
        .queue_event(EventPayload::KeyPress {
            key: Key::Character('g'),
            modifiers: Modifiers::NONE,
        })
        .unwrap();

    let initial_polls = Rc::new(RefCell::new(0usize));
    let polls = Rc::clone(&initial_polls);
    scene.set_hold_controller(move |kind, stage: &Stage, _time| {
        if kind == HoldKind::Initial {
            assert_eq!(
                stage.get_center(mob)[0],
                5.0,
                "queued event drains before the paused host is polled"
            );
            let mut count = polls.borrow_mut();
            *count += 1;
            if *count == 1 {
                Ok(HoldDecision::Continue)
            } else {
                Ok(HoldDecision::Release)
            }
        } else {
            Ok(HoldDecision::Release)
        }
    });

    let mut sink = PacketSink::default();
    scene.wait(Some(1.0), &mut sink).unwrap();
    assert_eq!(*initial_polls.borrow(), 2);
    assert_eq!(sink.packets.len(), 1);
    assert_eq!(sink.packets[0].0, CaptureReason::PresenterHold);
    assert_eq!(
        sink.packets[0].1.materialize_stage().get_center(mob)[0],
        5.0
    );
    assert_eq!(scene.recorded_events()[0].timestamp, RationalTime::zero(1));
}
