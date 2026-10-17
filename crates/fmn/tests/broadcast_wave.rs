//! Public effect composition checks the native and ordinary scene drivers.
use fmn::prelude::*;
use fmn_anim::Animation;
use fmn_scene::{CaptureReason, IntegrationError, RuntimeConfig, SceneSink};

struct Effects;
impl SceneConstruct for Effects {
    fn construct(&mut self, stage: &mut Stage<'_>) -> fmn::Result<()> {
        let mut circles = Vec::new();
        for radius in [0.5, 1.0, 1.5] {
            circles.push(stage.add(Circle::new().radius(radius).color(BLUE))?);
        }
        let raw = stage.scene_mut().stage_mut();
        let mut restores: Vec<Box<dyn Animation>> = Vec::new();
        for &circle in &circles {
            raw.save_state(circle)?;
            raw.scale(circle, 0.1);
            restores.push(Box::new(fmn_anim::restore(raw, circle)?));
        }
        let broadcast = fmn_anim::broadcast(raw, restores, 1.0, 0.5, true)?;
        stage.play_prepared(vec![Box::new(broadcast)])?;
        let raw = stage.scene_mut().stage_mut();
        for root in raw.roots() {
            let visible = raw.family(*root);
            assert!(circles.iter().all(|c| !visible.contains(c)));
        }
        let line = raw.add(fmn::mobject::Mobject::vector_from_points(&[
            [0.0, -1.0, 0.0],
            [0.0, 0.0, 0.0],
            [0.0, 1.0, 0.0],
        ]));
        raw.add_to_scene(line)?;
        let mut wave = fmn_anim::apply_wave(raw, line, [1.0, 0.0, 0.0], 0.5);
        wave.state_mut().config.rate_func = RateFunc::linear();
        stage.play_prepared(vec![Box::new(wave)])?;
        stage.wait(0.5)?;
        Ok(())
    }
}

struct Capture {
    native: bool,
    programs: usize,
    frames: Vec<Vec<u64>>,
}
impl Capture {
    fn record(&mut self, stage: fmn::mobject::Stage) {
        let mut row = vec![stage.time().to_bits()];
        for &root in stage.roots() {
            let family = stage.family(root);
            row.push(family.len() as u64);
            for member in family {
                let points = stage.get_points(member).unwrap();
                row.push(points.len() as u64);
                for point in points {
                    assert!(point.iter().all(|x| x.is_finite()));
                    row.extend(point.map(f64::to_bits));
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
fn broadcast_cleanup_and_vertical_wave_keep_finite_equivalent_public_frames() {
    let mut reference = None;
    for native in [false, true] {
        let mut capture = Capture {
            native,
            programs: 0,
            frames: Vec::new(),
        };
        let completed = fmn::run_scene(
            &mut Effects,
            RuntimeConfig {
                fps: 8,
                ..RuntimeConfig::default()
            },
            67,
            &mut capture,
        )
        .unwrap();
        assert_eq!(completed.report().time.frames(), 20);
        assert_eq!(capture.frames.len(), 20);
        assert_eq!(capture.programs, 2 * usize::from(native));
        if let Some(reference) = &reference {
            assert_eq!(&capture.frames, reference);
        } else {
            reference = Some(capture.frames);
        }
    }
}
