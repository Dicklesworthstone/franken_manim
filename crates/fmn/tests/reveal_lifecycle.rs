//! Reveal cleanup must leave objects usable in subsequent public scene plays.
use fmn::prelude::*;
use fmn_anim::{Animation, show_submobjects_one_by_one};
use fmn_scene::{CaptureReason, IntegrationError, RuntimeConfig, SceneSink};

struct RevealScene;
impl SceneConstruct for RevealScene {
    fn construct(&mut self, stage: &mut Stage<'_>) -> fmn::Result<()> {
        let a = stage.add(Circle::new().radius(0.3).color(BLUE))?;
        let b = stage.add(Circle::new().radius(0.4).color(RED))?;
        let raw = stage.scene_mut().stage_mut();
        raw.shift(a, [-1.0, 0.0, 0.0]);
        raw.shift(b, [1.0, 0.0, 0.0]);
        raw.remove_from_scene(a);
        raw.remove_from_scene(b);
        let root = raw.add(fmn::mobject::Mobject::new());
        raw.attach(root, a)?;
        raw.attach(root, b)?;
        raw.add_to_scene(root)?;
        let mut reveal = show_submobjects_one_by_one(raw, root)?;
        reveal.state_mut().config = AnimConfig {
            run_time: 1.0,
            rate_func: RateFunc::linear(),
            suspend_mobject_updating: true,
            ..AnimConfig::default()
        };
        stage.play_prepared(vec![Box::new(reveal)])?;
        let raw = stage.scene_mut().stage_mut();
        assert_eq!(raw.get(root).unwrap().submobjects(), &[b]);
        for mob in [a, b] {
            assert!(!raw.is_animating(mob));
            assert!(!raw.is_updating_suspended(mob));
        }
        raw.add_to_scene(a)?;
        let initial = raw.get_center(a)[0];
        raw.add_dt_updater(
            a,
            |s, m, dt| {
                s.shift(m, [dt, 0.0, 0.0]);
            },
            false,
        )?;
        stage.wait(0.5)?;
        assert!((stage.scene_mut().stage_mut().get_center(a)[0] - initial - 0.5).abs() < 1e-12);
        Ok(())
    }
}

struct Capture {
    native: bool,
    programs: usize,
    frames: Vec<Vec<String>>,
}
impl Capture {
    fn record(&mut self, stage: fmn::mobject::Stage) {
        let mut row = vec![format!("time:{:x}", stage.time().to_bits())];
        for &root in stage.roots() {
            let family = stage.family(root);
            row.push(format!("family:{}", family.len()));
            for mob in family {
                let e = stage.get(mob).unwrap();
                row.push(format!(
                    "{:?}/{:?}/{:?}",
                    e.placement(),
                    e.uniforms(),
                    (stage.is_animating(mob), stage.is_updating_suspended(mob))
                ));
                for field in e.buffer.schema().fields() {
                    row.push(format!(
                        "{}:{:?}",
                        field.name,
                        e.buffer.read_column(&field.name)
                    ));
                }
            }
        }
        self.frames.push(row);
    }
}
impl SceneSink for Capture {
    fn accepts_pure_segments(&self) -> bool {
        self.native
    }
    fn capture_pure_segment(
        &mut self,
        segment: &fmn_anim::pure_frame::PureSegment,
    ) -> Result<(), IntegrationError> {
        self.programs += 1;
        for frame in 1..=segment.frame_count() {
            self.record(segment.job(frame).unwrap().materialize());
        }
        Ok(())
    }
    fn capture(&mut self, _: CaptureReason, packet: FramePacket) -> Result<(), IntegrationError> {
        self.record(packet.materialize_stage());
        Ok(())
    }
}

#[test]
fn public_one_by_one_reveal_reaches_last_child_and_hidden_children_can_move_again() {
    let mut reference = None;
    for native in [false, true] {
        let mut sink = Capture {
            native,
            programs: 0,
            frames: Vec::new(),
        };
        let completed = fmn::run_scene(
            &mut RevealScene,
            RuntimeConfig {
                fps: 8,
                ..RuntimeConfig::default()
            },
            19,
            &mut sink,
        )
        .unwrap();
        assert_eq!(completed.report().time.frames(), 12);
        assert_eq!(sink.frames.len(), 12);
        assert_eq!(sink.programs, usize::from(native));
        if let Some(reference) = &reference {
            assert_eq!(&sink.frames, reference);
        } else {
            reference = Some(sink.frames);
        }
    }
}
