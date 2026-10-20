//! Interactive controls must participate in ordinary native scenes and replay.

use std::cell::{Cell, RefCell};
use std::path::Path;
use std::rc::Rc;
use std::sync::Arc;

use fmn::controls::{
    add_button, add_checkbox, add_enable_disable_button, add_slider, add_try_button,
};
use fmn::prelude::*;
use fmn_codec::{PngLimits, decode_png};
use fmn_config::config::{DeterminismMode, ThreadPolicy};
use fmn_library::{Button, Checkbox, EnableDisableButton, LinearNumberSlider};
use fmn_mobject::Stage as Arena;
use fmn_platform::fs::{FileSystem, VirtualFs};
use fmn_scene::{
    EventListener, EventPayload, EventPropagation, EventTarget, EventType, Modifiers, MouseButton,
};

fn press(point: Vec3) -> EventPayload {
    EventPayload::MousePress {
        point,
        button: MouseButton::Left,
        modifiers: Modifiers::NONE,
    }
}

fn drag(point: Vec3) -> EventPayload {
    EventPayload::MouseDrag {
        point,
        delta: [0.0; 3],
        button: MouseButton::Left,
        modifiers: Modifiers::NONE,
    }
}

fn release(point: Vec3) -> EventPayload {
    EventPayload::MouseRelease {
        point,
        button: MouseButton::Left,
        modifiers: Modifiers::NONE,
    }
}

fn dispatch(scene: &mut Scene, payload: EventPayload) {
    scene.queue_event(payload).unwrap();
    assert_eq!(scene.dispatch_pending_events().unwrap(), 1);
    assert!(scene.stage().updater_error().is_none());
}

fn close(actual: Vec3, expected: Vec3) {
    for axis in 0..3 {
        assert!(
            (actual[axis] - expected[axis]).abs() < 2e-6,
            "{actual:?} != {expected:?}"
        );
    }
}

fn lerp(start: Vec3, end: Vec3, fraction: f64) -> Vec3 {
    std::array::from_fn(|axis| start[axis] + fraction * (end[axis] - start[axis]))
}

fn geometry(stage: &Arena, root: Mob) -> Vec<Vec<Vec3>> {
    stage
        .family(root)
        .into_iter()
        .filter_map(|member| stage.get_points(member).filter(|points| !points.is_empty()))
        .collect()
}

fn same_geometry(actual: &[Vec<Vec3>], expected: &[Vec<Vec3>]) {
    assert_eq!(actual.len(), expected.len());
    for (actual, expected) in actual.iter().zip(expected) {
        assert_eq!(actual.len(), expected.len());
        for (&actual, &expected) in actual.iter().zip(expected) {
            close(actual, expected);
        }
    }
}

#[test]
fn transformed_slider_uses_live_axis_snapping_and_capture_through_release() {
    let mut scene = Scene::default();
    let slider = add_slider(
        &mut scene,
        LinearNumberSlider::new(0.0)
            .unwrap()
            .range(0.0, 10.0)
            .unwrap()
            .step(2.0)
            .unwrap(),
    )
    .unwrap();
    slider.set_value(scene.stage_mut(), 0.0).unwrap();
    let (handle, axis) = (slider.components()[1], slider.components()[2]);
    scene.stage_mut().scale(slider.container(), 2.0);
    scene.stage_mut().rotate(
        slider.container(),
        std::f64::consts::FRAC_PI_2,
        OUT,
        None,
        None,
    );
    scene.stage_mut().shift(slider.container(), [2.0, 1.0, 0.0]);
    let points = scene.stage().get_points(axis).unwrap();
    let (start, end) = (*points.first().unwrap(), *points.last().unwrap());
    let width = scene.stage().get_width(handle);
    let initial = scene.stage().get_center(handle);
    dispatch(&mut scene, press(initial));
    // Far beyond the original handle bounds: only captured dragging can reach it.
    dispatch(&mut scene, drag(lerp(start, end, 2.0)));
    assert_eq!(slider.value(scene.stage()).unwrap(), 10.0);
    close(scene.stage().get_center(handle), end);
    dispatch(&mut scene, drag(lerp(start, end, 0.73)));
    assert_eq!(slider.value(scene.stage()).unwrap(), 6.0);
    close(scene.stage().get_center(handle), lerp(start, end, 0.6));
    assert!((scene.stage().get_width(handle) - width).abs() < 1e-6);
    dispatch(&mut scene, release([99.0, 99.0, 0.0]));
    dispatch(&mut scene, drag(start));
    assert_eq!(slider.value(scene.stage()).unwrap(), 6.0);
    assert_eq!(slider.components()[1], handle);
}

#[test]
fn checkbox_and_enable_toggle_keep_transformed_geometry_and_constructor_colors() {
    let mut scene = Scene::default();
    let checkbox = add_checkbox(&mut scene, Checkbox::new(false)).unwrap();
    let parts = checkbox.components().to_vec();
    scene.stage_mut().scale(checkbox.container(), 1.7);
    scene
        .stage_mut()
        .rotate(checkbox.container(), 0.4, OUT, None, None);
    scene
        .stage_mut()
        .shift(checkbox.container(), [-2.0, 0.7, 0.0]);
    let click = scene.stage().get_center(parts[0]);
    dispatch(&mut scene, press(click));
    assert_eq!(checkbox.value(scene.stage()).unwrap(), 1.0);
    assert_eq!(checkbox.components(), parts);
    let mut expected = Arena::new();
    let root = expected.add(Checkbox::new(true).composition());
    expected.scale(root, 1.7);
    expected.rotate(root, 0.4, OUT, None, None);
    expected.shift(root, [-2.0, 0.7, 0.0]);
    same_geometry(
        &geometry(scene.stage(), checkbox.visual()),
        &geometry(&expected, root),
    );

    let toggle = add_enable_disable_button(&mut scene, EnableDisableButton::new(false)).unwrap();
    let rect = toggle.components()[0];
    assert_eq!(
        scene.stage().get_fill_color(rect).unwrap().to_rgb8(),
        WHITE.to_rgb8()
    );
    scene
        .stage_mut()
        .shift(toggle.container(), [2.0, -0.7, 0.0]);
    let bounds = scene.stage().get_bounding_box(rect);
    let point = scene.stage().get_center(rect);
    dispatch(&mut scene, press(point));
    assert_eq!(toggle.value(scene.stage()).unwrap(), 1.0);
    assert_eq!(
        scene.stage().get_fill_color(rect).unwrap().to_rgb8(),
        GREEN.to_rgb8()
    );
    dispatch(&mut scene, press(point));
    assert_eq!(toggle.value(scene.stage()).unwrap(), 0.0);
    assert_eq!(
        scene.stage().get_fill_color(rect).unwrap().to_rgb8(),
        RED.to_rgb8()
    );
    assert_eq!(scene.stage().get_bounding_box(rect), bounds);
}

#[test]
fn copied_controls_remap_trackers_and_restore_independent_native_state() {
    let mut scene = Scene::default();
    let original = add_checkbox(&mut scene, Checkbox::new(false)).unwrap();
    let copied = scene
        .stage_mut()
        .copy_family_mapped(original.container())
        .unwrap();
    let copy = original.bind_copy(&mut scene, &copied).unwrap();
    assert_eq!(copy.tracker(), copied.get(original.tracker()).unwrap());
    scene.stage_mut().shift(copy.container(), [3.0, 0.0, 0.0]);
    let saved = scene.stage().snapshot();
    let point = scene.stage().get_center(copy.components()[0]);
    dispatch(&mut scene, press(point));
    assert_eq!(copy.value(scene.stage()).unwrap(), 1.0);
    assert_eq!(original.value(scene.stage()).unwrap(), 0.0);
    let changed = geometry(scene.stage(), copy.visual());
    scene.stage_mut().restore(&saved);
    assert_eq!(copy.value(scene.stage()).unwrap(), 0.0);
    assert_ne!(geometry(scene.stage(), copy.visual()), changed);
    // The restored model must accept the same transition again, rebuilding
    // the copy's mark rather than editing the original captured handles.
    scene
        .stage_mut()
        .set_tracker_value(copy.tracker(), 1.0)
        .unwrap();
    scene.stage_mut().update(0.0);
    same_geometry(&geometry(scene.stage(), copy.visual()), &changed);
    assert_eq!(original.value(scene.stage()).unwrap(), 0.0);
    assert_eq!(
        copy.components()[1],
        copied.get(original.components()[1]).unwrap()
    );
}

#[test]
fn unbinding_is_idempotent_foreign_safe_and_cleans_up_deleted_controls() {
    let mut first = Scene::default();
    let mut second = Scene::default();
    let mut control = add_checkbox(&mut first, Checkbox::new(false)).unwrap();
    let second_control = add_checkbox(&mut second, Checkbox::new(false)).unwrap();
    assert!(!control.unbind(&mut second));
    assert_eq!(second.event_dispatcher().listener_count(), 1);
    dispatch(&mut second, press(ORIGIN));
    assert_eq!(second_control.value(second.stage()).unwrap(), 1.0);
    dispatch(&mut first, press(ORIGIN));
    assert_eq!(control.value(first.stage()).unwrap(), 1.0);
    assert!(control.unbind(&mut first));
    assert!(!control.unbind(&mut first));
    let visible = geometry(first.stage(), control.visual());
    dispatch(&mut first, press(ORIGIN));
    first
        .stage_mut()
        .set_tracker_value(control.tracker(), 0.0)
        .unwrap();
    first.stage_mut().update(0.0);
    assert_eq!(geometry(first.stage(), control.visual()), visible);
    assert!(first.stage().contains(control.container()));
    assert_eq!(first.event_dispatcher().listener_count(), 0);

    let mut deleted = add_checkbox(&mut first, Checkbox::new(false)).unwrap();
    first.stage_mut().delete(deleted.container()).unwrap();
    assert!(deleted.unbind(&mut first));
    assert_eq!(first.event_dispatcher().listener_count(), 0);
}

#[test]
fn unrepresentable_constructor_geometry_is_refused_before_scene_publication() {
    let mut scene = Scene::default();
    let before = scene.stage().snapshot_bytes().unwrap();
    let slider = LinearNumberSlider::new(0.0).unwrap().handle_radius(1e100);
    assert!(add_slider(&mut scene, slider).is_err());
    assert_eq!(scene.stage().snapshot_bytes().unwrap(), before);
    assert_eq!(scene.event_dispatcher().listener_count(), 0);
}

#[test]
fn buttons_deliver_the_receiving_copy_content_and_ignore_removed_families() {
    let book = FontBook::bundled().unwrap();
    let mut scene = Scene::default();
    let calls = Rc::new(RefCell::new(Vec::new()));
    let log = Rc::clone(&calls);
    let button = add_button(
        &mut scene,
        Button::of(Circle::new().radius(0.2).build()),
        &book,
        move |_, content| {
            log.borrow_mut().push(content);
        },
    )
    .unwrap();
    scene.stage_mut().shift(button.container(), [2.0, 0.0, 0.0]);
    dispatch(&mut scene, press(ORIGIN));
    assert!(calls.borrow().is_empty());
    dispatch(&mut scene, press([2.0, 0.0, 0.0]));
    let copied = scene
        .stage_mut()
        .copy_family_mapped(button.container())
        .unwrap();
    let copy = button.bind_copy(&mut scene, &copied).unwrap();
    scene.stage_mut().shift(copy.container(), [-4.0, 0.0, 0.0]);
    dispatch(&mut scene, press([-2.0, 0.0, 0.0]));
    assert_eq!(*calls.borrow(), vec![button.content(), copy.content()]);
    scene.remove(&[copy.container()]);
    dispatch(&mut scene, press([-2.0, 0.0, 0.0]));
    assert_eq!(calls.borrow().len(), 2);
}

#[derive(Default)]
struct CountFrames(usize);
impl SceneSink for CountFrames {
    fn capture(
        &mut self,
        _: CaptureReason,
        _: FramePacket,
    ) -> std::result::Result<(), IntegrationError> {
        self.0 += 1;
        Ok(())
    }
}

#[test]
fn failed_button_callback_blocks_frames_and_acknowledgment_does_not_repeat_the_click() {
    let book = FontBook::bundled().unwrap();
    let mut scene = Scene::default();
    let calls = Rc::new(Cell::new(0));
    let count = Rc::clone(&calls);
    let button = add_try_button(&mut scene, Button::new("Run"), &book, move |_, _| {
        count.set(count.get() + 1);
        if count.get() == 1 {
            Err(UpdaterError::callback("button action", "refused input"))
        } else {
            Ok(())
        }
    })
    .unwrap();
    let later = Rc::new(Cell::new(0));
    let listener_count = Rc::clone(&later);
    scene
        .event_dispatcher_mut()
        .add_listener(EventListener::new(
            EventType::MousePress,
            EventTarget::Global,
            move |_, _, _, _| {
                listener_count.set(listener_count.get() + 1);
                EventPropagation::Continue
            },
        ))
        .unwrap();
    scene.queue_event(press(ORIGIN)).unwrap();
    let mut sink = CountFrames::default();
    let error = scene.show(&mut sink).unwrap_err();
    assert!(
        matches!(error, SceneError::Animation(AnimError::Updater(ref failure)) if failure.mobject == button.container())
    );
    assert_eq!((calls.get(), later.get(), sink.0), (1, 0, 0));
    assert!(scene.show(&mut sink).is_err());
    assert_eq!(calls.get(), 1);
    scene.stage_mut().take_updater_error().unwrap().unwrap();
    scene.show(&mut sink).unwrap();
    assert_eq!((calls.get(), sink.0), (1, 1));
    scene.queue_event(press(ORIGIN)).unwrap();
    scene.show(&mut sink).unwrap();
    assert_eq!((calls.get(), sink.0), (2, 2));
}

#[test]
fn slider_readout_growth_preserves_live_placement_paint_and_atomic_refusals() {
    let mut scene = Scene::default();
    let book = Rc::new(FontBook::bundled().unwrap());
    let slider = add_slider(
        &mut scene,
        LinearNumberSlider::new(9.0)
            .unwrap()
            .range(-200.0, 200.0)
            .unwrap()
            .with_value_readout(book, 1)
            .unwrap(),
    )
    .unwrap();
    let readout = slider.components()[3];
    scene.stage_mut().scale(slider.container(), 1.3);
    scene
        .stage_mut()
        .rotate(slider.container(), 0.35, OUT, None, None);
    scene
        .stage_mut()
        .shift(slider.container(), [1.0, -0.4, 0.0]);
    scene
        .stage_mut()
        .set_color(readout, YELLOW, Some(0.7), true);
    let placement = scene.stage().placement(readout).unwrap();
    let before_count = scene.stage().family(readout).len();
    slider.set_value(scene.stage_mut(), -125.5).unwrap();
    assert!(scene.stage().family(readout).len() > before_count);
    for member in scene.stage().family(readout) {
        assert!(
            scene
                .stage()
                .placement(member)
                .unwrap()
                .same_bits(placement)
        );
        if scene.stage().get(member).unwrap().buffer.is_empty() {
            continue;
        }
        assert_eq!(
            scene.stage().get_fill_color(member).unwrap().to_rgb8(),
            YELLOW.to_rgb8()
        );
        assert!((scene.stage().get_fill_opacity(member).unwrap() - 0.7).abs() < 1e-6);
    }
    let valid = geometry(scene.stage(), slider.visual());
    assert!(slider.set_value(scene.stage_mut(), f64::NAN).is_err());
    assert_eq!(slider.value(scene.stage()).unwrap(), -125.5);
    assert_eq!(geometry(scene.stage(), slider.visual()), valid);
    scene
        .stage_mut()
        .set_tracker_value(slider.tracker(), f64::INFINITY)
        .unwrap();
    let mut sink = CountFrames::default();
    assert!(matches!(
        scene.show(&mut sink),
        Err(SceneError::Animation(AnimError::Updater(_)))
    ));
    assert_eq!(sink.0, 0);
    assert_eq!(geometry(scene.stage(), slider.visual()), valid);
    scene.stage_mut().take_updater_error().unwrap().unwrap();
    slider.set_value(scene.stage_mut(), 1000.0).unwrap();
    assert_eq!(slider.value(scene.stage()).unwrap(), 200.0);
    scene.show(&mut sink).unwrap();
    assert_eq!(sink.0, 1);
}

#[test]
fn recorded_pointer_events_replay_to_identical_values_and_geometry() {
    fn setup() -> (
        Scene,
        fmn::controls::ScalarControlBinding,
        fmn::controls::ScalarControlBinding,
    ) {
        let mut scene = Scene::default();
        let checkbox = add_checkbox(&mut scene, Checkbox::new(false)).unwrap();
        scene
            .stage_mut()
            .shift(checkbox.container(), [-2.0, 0.0, 0.0]);
        let slider = add_slider(
            &mut scene,
            LinearNumberSlider::new(0.0)
                .unwrap()
                .range(0.0, 10.0)
                .unwrap()
                .step(1.0)
                .unwrap(),
        )
        .unwrap();
        scene.stage_mut().shift(slider.container(), [1.0, 0.0, 0.0]);
        (scene, checkbox, slider)
    }
    let (mut authored, checkbox, slider) = setup();
    for event in [
        press([-2.0, 0.0, 0.0]),
        release([-2.0, 0.0, 0.0]),
        press([1.0, 0.0, 0.0]),
        drag([1.6, 0.0, 0.0]),
        release([4.0, 0.0, 0.0]),
    ] {
        dispatch(&mut authored, event);
    }
    let recorded = authored.recorded_events().to_vec();
    assert_eq!(recorded.len(), 5);
    let (mut replay, replay_checkbox, replay_slider) = setup();
    replay.queue_replay_events(&recorded).unwrap();
    replay.show(&mut CountFrames::default()).unwrap();
    assert_eq!(replay.recorded_events(), recorded);
    assert_eq!(
        replay_checkbox.value(replay.stage()).unwrap(),
        checkbox.value(authored.stage()).unwrap()
    );
    assert_eq!(
        replay_slider.value(replay.stage()).unwrap(),
        slider.value(authored.stage()).unwrap()
    );
    same_geometry(
        &geometry(replay.stage(), replay_checkbox.visual()),
        &geometry(authored.stage(), checkbox.visual()),
    );
    same_geometry(
        &geometry(replay.stage(), replay_slider.visual()),
        &geometry(authored.stage(), slider.visual()),
    );
}

struct AnimatedSlider {
    handles: Rc<RefCell<Option<(Mob, Mob)>>>,
}
impl SceneConstruct for AnimatedSlider {
    fn construct(&mut self, stage: &mut Stage<'_>) -> fmn::Result<()> {
        let slider = add_slider(
            stage.scene_mut(),
            LinearNumberSlider::new(0.0)?.range(0.0, 10.0)?,
        )?;
        slider.set_value(stage.arena_mut(), 0.0)?;
        *self.handles.borrow_mut() = Some((slider.tracker(), slider.components()[1]));
        stage.play(
            slider
                .tracker()
                .animate()
                .set_anim_args(AnimateArgs {
                    run_time: Some(1.0),
                    rate_func: Some(fmn::core::rate::linear),
                    ..AnimateArgs::default()
                })?
                .set_value(10.0)?,
        )?;
        Ok(())
    }
}

struct SliderFrames {
    handles: Rc<RefCell<Option<(Mob, Mob)>>>,
    values: Vec<f64>,
}
impl SceneSink for SliderFrames {
    fn capture(
        &mut self,
        _: CaptureReason,
        packet: FramePacket,
    ) -> std::result::Result<(), IntegrationError> {
        let (tracker, handle) = self.handles.borrow().unwrap();
        let stage = packet.materialize_stage();
        let value = stage.tracker_value(tracker).unwrap();
        close(stage.get_center(handle), [-1.0 + value / 5.0, 0.0, 0.0]);
        self.values.push(value);
        Ok(())
    }
}

#[test]
fn tracker_animation_moves_the_visible_slider_on_actual_intermediate_frames() {
    let handles = Rc::new(RefCell::new(None));
    let mut authored = AnimatedSlider {
        handles: Rc::clone(&handles),
    };
    let mut frames = SliderFrames {
        handles,
        values: Vec::new(),
    };
    fmn::run_scene(
        &mut authored,
        RuntimeConfig {
            fps: 8,
            ..RuntimeConfig::default()
        },
        0,
        &mut frames,
    )
    .unwrap();
    assert!(
        frames
            .values
            .iter()
            .any(|&value| value > 0.0 && value < 10.0)
    );
    assert_eq!(*frames.values.last().unwrap(), 10.0);
}

#[test]
fn event_driven_native_png_matches_an_independently_authored_endpoint() {
    struct Controls {
        interactive: bool,
    }
    impl SceneConstruct for Controls {
        fn construct(&mut self, stage: &mut Stage<'_>) -> fmn::Result<()> {
            if self.interactive {
                let checkbox = add_checkbox(stage.scene_mut(), Checkbox::new(false))?;
                stage.shift(checkbox.container(), [-2.0, 0.0, 0.0]);
                let slider = add_slider(
                    stage.scene_mut(),
                    LinearNumberSlider::new(0.0)?.range(0.0, 10.0)?.step(1.0)?,
                )?;
                stage.shift(slider.container(), [1.0, 0.0, 0.0]);
                for event in [
                    press([-2.0, 0.0, 0.0]),
                    release([-2.0, 0.0, 0.0]),
                    press([1.0, 0.0, 0.0]),
                    drag([4.0, 0.0, 0.0]),
                    release([4.0, 0.0, 0.0]),
                ] {
                    stage
                        .scene_mut()
                        .queue_event(event)
                        .map_err(SceneError::from)?;
                }
                stage.wait(0.25)?;
            } else {
                let checkbox = stage.add(Checkbox::new(true))?;
                stage.shift(checkbox, [-2.0, 0.0, 0.0]);
                let mut slider = LinearNumberSlider::new(0.0)?.range(0.0, 10.0)?.step(1.0)?;
                slider.set_value(10.0)?;
                let slider = stage.add(slider)?;
                stage.shift(slider, [1.0, 0.0, 0.0]);
            }
            Ok(())
        }
    }
    let fs = Arc::new(VirtualFs::new());
    for (path, interactive) in [("/controls.png", true), ("/expected-controls.png", false)] {
        let mut options = RenderOptions::new(path).unwrap();
        options.format = RenderFormat::Png;
        options.config.camera.resolution = (96, 64);
        options.config.camera.fps = 8;
        options.config.sizes.frame_height = 4.0;
        options.config.determinism.mode = DeterminismMode::Certified;
        options.config.render.threads = ThreadPolicy::Fixed(1);
        render_with_fs(&mut Controls { interactive }, options, fs.clone()).unwrap();
    }
    let actual = decode_png(
        &fs.read(Path::new("/controls.png")).unwrap(),
        &PngLimits::default(),
    )
    .unwrap();
    let expected = decode_png(
        &fs.read(Path::new("/expected-controls.png")).unwrap(),
        &PngLimits::default(),
    )
    .unwrap();
    assert_eq!(actual.rgba, expected.rgba);
    assert!(
        actual
            .rgba
            .as_chunks::<4>()
            .0
            .iter()
            .any(|pixel| pixel[0] > 32 || pixel[1] > 32 || pixel[2] > 32)
    );
}
