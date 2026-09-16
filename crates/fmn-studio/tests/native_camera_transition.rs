//! The camera convenience API lowers to the existing Transform lifecycle.

use std::cell::Cell;
use std::rc::Rc;

use fmn_render::CameraConfig;
use fmn_scene::studio_bridge::FramePacket;
use fmn_scene::{CameraRig, CaptureReason, IntegrationError, PlayOverrides, RuntimeConfig, Scene, SceneSink};

#[derive(Default)]
struct Captures(Vec<FramePacket>);
impl SceneSink for Captures {
    fn capture(&mut self, _: CaptureReason, packet: FramePacket) -> Result<(), IntegrationError> {
        self.0.push(packet); Ok(())
    }
}

#[test]
fn equivalent_opposite_quaternions_do_not_interpolate_through_a_zero_orientation() {
    let base = CameraConfig::default();
    let mut scene = Scene::new(RuntimeConfig { fps: 8, ..RuntimeConfig::default() }, 23).unwrap();
    let rig = CameraRig::new(&mut scene, &base).unwrap();
    let mut target = base.clone();
    target.frame.set_orientation([0.0, 0.0, 0.0, -1.0]).unwrap();
    target.frame.set_center([1.0, 2.0, 0.0]).unwrap();
    target.frame.set_width(8.0).unwrap();
    target.light_source_position = [2.0, 3.0, 8.0];
    let animation = rig.animate_to(&mut scene, &target).unwrap();
    assert_eq!(scene.stage().roots(), &[rig.root()], "detached target is not drawn");
    assert_eq!(rig.sample(scene.stage(), &base).unwrap().frame.center(), [0.0; 3]);
    let mut captures = Captures::default();
    scene.play(vec![Box::new(animation)], PlayOverrides { run_time: Some(0.5), ..PlayOverrides::default() }, &mut captures).unwrap();
    assert_eq!(captures.0.len(), 4);
    for packet in &captures.0 {
        let sampled = rig.sample(&packet.materialize_stage(), &base).unwrap();
        assert_eq!(sampled.frame.orientation(), [0.0, 0.0, 0.0, 1.0]);
    }
    let final_pose = rig.sample(&captures.0.last().unwrap().materialize_stage(), &base).unwrap();
    assert_eq!(final_pose.frame.center(), [1.0, 2.0, 0.0]);
    assert_eq!(final_pose.frame.width(), 8.0);
    assert_eq!(final_pose.light_source_position, target.light_source_position);
}

#[test]
fn target_pose_does_not_inherit_source_updaters() {
    let base = CameraConfig::default();
    let mut scene = Scene::new(RuntimeConfig { fps: 8, ..RuntimeConfig::default() }, 23).unwrap();
    let rig = CameraRig::new(&mut scene, &base).unwrap();
    let calls = Rc::new(Cell::new(0));
    let seen = Rc::clone(&calls);
    scene.stage_mut().add_updater(rig.width(), move |stage, width| {
        seen.set(seen.get() + 1);
        stage.set_tracker_value(width, stage.tracker_value(width).unwrap() + 0.1).unwrap();
    }, false).unwrap();
    let mut target = base.clone(); target.frame.set_width(2.0).unwrap();
    let animation = rig.animate_to(&mut scene, &target).unwrap();
    let mut captures = Captures::default();
    scene.play(vec![Box::new(animation)], PlayOverrides { run_time: Some(0.5), ..PlayOverrides::default() }, &mut captures).unwrap();
    let last = rig.sample(&captures.0.last().unwrap().materialize_stage(), &base).unwrap();
    assert_eq!(last.frame.width(), 2.0, "callbacks must not move the destination");
    assert!(calls.get() > 0, "source updater resumes through the ordinary lifecycle");
}

#[test]
fn invalid_target_is_refused_before_allocating_or_changing_source_state() {
    let base = CameraConfig::default();
    let mut scene = Scene::default();
    let rig = CameraRig::new(&mut scene, &base).unwrap();
    let before = scene.state_bytes().unwrap();
    let mut invalid = base.clone(); invalid.resolution = (0, 0);
    assert!(rig.animate_to(&mut scene, &invalid).is_err());
    assert_eq!(scene.state_bytes().unwrap(), before);
    assert!(rig.animate_to(&mut Scene::default(), &base).is_err());
}
