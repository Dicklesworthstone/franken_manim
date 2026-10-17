//! Portable PCM is content-bound to observed pictures, never silently dropped.
use fmn_anim::FramePacket;
use fmn_hash::{
    serial::{Limits, Reader, UnknownPolicy, Writer},
    sha256,
};
use fmn_mobject::Mobject;
use fmn_render::camera::CameraSample;
use fmn_render::{Camera, CameraConfig};
use fmn_scene::recording::SceneBundleRecorder;
use fmn_scene::timeline_bundle::{
    BundleReadError, BundleSoundtrack, CAMERA_TIMELINE_BUNDLE_SCHEMA,
    SOUNDTRACK_TIMELINE_BUNDLE_SCHEMA, TimelineBundle, TimelineFrameCache,
    attach_bundle_soundtrack, bundle_engine_version,
};
use fmn_scene::{
    BundleExportLimits, CaptureReason, IntegrationError, LifecycleEvent, RuntimeConfig, Scene,
    SceneSink,
};

struct Capture {
    recorder: SceneBundleRecorder,
    camera: bool,
    index: u32,
}
impl SceneSink for Capture {
    fn event(&mut self, event: LifecycleEvent) -> Result<(), IntegrationError> {
        self.recorder.event(event)
    }
    fn capture(
        &mut self,
        reason: CaptureReason,
        packet: FramePacket,
    ) -> Result<(), IntegrationError> {
        self.index += 1;
        if self.camera {
            let mut config = CameraConfig {
                fps: 8,
                resolution: (64, 36),
                ..CameraConfig::default()
            };
            config
                .frame
                .set_center([f64::from(self.index) / 4.0, 0.0, 0.0])
                .unwrap();
            let camera = Camera::new(config).unwrap();
            self.recorder.capture_with_camera(reason, packet, &camera)
        } else {
            self.recorder.capture(reason, packet)
        }
    }
}
fn pictures(camera: bool) -> Vec<u8> {
    let mut scene = Scene::new(
        RuntimeConfig {
            fps: 8,
            ..RuntimeConfig::default()
        },
        7,
    )
    .unwrap();
    let mob = scene
        .add_mobject(Mobject::vector_from_points(&[
            [0.0, 0.0, 0.0],
            [1.0, 1.0, 0.0],
            [2.0, 0.0, 0.0],
        ]))
        .unwrap();
    scene
        .stage_mut()
        .add_dt_updater(
            mob,
            |s, m, dt| {
                s.shift(m, [dt, 0.0, 0.0]);
            },
            false,
        )
        .unwrap();
    let limits = BundleExportLimits::default();
    let recorder = if camera {
        SceneBundleRecorder::new_render_only_with_camera(8, limits)
    } else {
        SceneBundleRecorder::new_render_only(8, limits)
    }
    .unwrap();
    let mut sink = Capture {
        recorder,
        camera,
        index: 0,
    };
    scene.wait(Some(0.5), &mut sink).unwrap();
    sink.recorder.finish().unwrap().bytes
}
fn track() -> BundleSoundtrack {
    BundleSoundtrack::new(11, 2, vec![0.0, -0.0, 0.125, -0.25, 0.5, -0.5, 1.0, -1.0]).unwrap()
}
fn envelope(pictures: &[u8], rate: u32, channels: u16, frames: u64, samples: &[u32]) -> Vec<u8> {
    let mut w = Writer::new(SOUNDTRACK_TIMELINE_BUNDLE_SCHEMA);
    w.put_str(&bundle_engine_version())
        .put_bytes(pictures)
        .put_u32(rate)
        .put_u16(channels)
        .put_u64(frames);
    for &bits in samples {
        w.put_u32(bits);
    }
    w.finish().unwrap()
}

#[test]
fn pcm_construction_has_explicit_packing_and_canonical_normalized_bits() {
    for (rate, ch, samples) in [
        (0, 1, vec![]),
        (8, 0, vec![]),
        (8, 3, vec![]),
        (8, 2, vec![0.0]),
        (u32::MAX, 2, vec![0.0, 0.0]),
        (8, 1, vec![f32::NAN]),
        (8, 1, vec![f32::INFINITY]),
        (8, 1, vec![1.1]),
        (8, 1, vec![-1.1]),
    ] {
        assert!(matches!(
            BundleSoundtrack::new(rate, ch, samples),
            Err(BundleReadError::Soundtrack(_))
        ));
    }
    assert_eq!(track().samples()[1].to_bits(), 0);
    assert_eq!(
        (
            track().sample_rate(),
            track().channels(),
            track().sample_frames()
        ),
        (11, 2, 4)
    );
    assert_eq!(track().clone().into_samples(), track().samples());
    assert_eq!(
        BundleSoundtrack::new(8, 1, vec![]).unwrap().sample_frames(),
        0
    );
}

#[test]
fn audible_recordings_preserve_pcm_camera_and_every_picture_after_explicit_transfer() {
    for camera in [false, true] {
        let visual = pictures(camera);
        let expected = TimelineBundle::from_bytes(&visual).unwrap();
        let bytes = attach_bundle_soundtrack(&visual, &track(), Limits::DEFAULT.max_total).unwrap();
        assert_eq!(
            bytes,
            attach_bundle_soundtrack(&pictures(camera), &track(), Limits::DEFAULT.max_total)
                .unwrap()
        );
        assert_ne!(sha256(&bytes), sha256(&visual));
        let mut bundle = TimelineBundle::from_bytes(&bytes).unwrap();
        assert!(bundle.has_soundtrack());
        assert_eq!(bundle.soundtrack(), Some(&track()));
        assert_eq!(
            (
                bundle.fps(),
                bundle.frame_count(),
                bundle.has_camera_track()
            ),
            (8, 4, camera)
        );
        assert!(matches!(
            bundle.stage_at_with_camera(0, (64, 36)),
            Err(BundleReadError::SoundtrackRequired)
        ));
        assert!(
            TimelineBundle::from_bytes(&bytes)
                .unwrap()
                .into_shared()
                .is_err()
        );
        assert_eq!(bundle.take_soundtrack(), Some(track()));
        assert!(!bundle.has_soundtrack());
        assert!(bundle.take_soundtrack().is_none());
        for frame in [3, 0, 2, 1, 0] {
            let (a, ac) = bundle.stage_at_with_camera(frame, (64, 36)).unwrap();
            let (b, bc) = expected.stage_at_with_camera(frame, (64, 36)).unwrap();
            assert_eq!(
                a.snapshot().to_render_bytes().unwrap(),
                b.snapshot().to_render_bytes().unwrap()
            );
            assert_eq!(
                ac.as_ref().map(|c| CameraSample::capture(c).unwrap()),
                bc.as_ref().map(|c| CameraSample::capture(c).unwrap())
            );
        }
        let shared = bundle.into_shared().unwrap();
        for workers in [1, 4, 16] {
            let output = std::thread::scope(|scope| {
                let mut jobs = Vec::new();
                for worker in 0..workers {
                    let shared = &shared;
                    jobs.push(scope.spawn(move || {
                        let mut cache = TimelineFrameCache::default();
                        let mut out = Vec::new();
                        for i in (0..4).rev().filter(|i| i % workers == worker) {
                            let (s, c) = cache
                                .materialize_with_camera(&shared.frame_job(i).unwrap(), (64, 36))
                                .unwrap();
                            out.push((
                                i,
                                s.snapshot().to_render_bytes().unwrap(),
                                c.map(|c| CameraSample::capture(&c).unwrap()),
                            ));
                        }
                        out
                    }));
                }
                jobs.into_iter()
                    .flat_map(|j| j.join().unwrap())
                    .collect::<Vec<_>>()
            });
            assert_eq!(output.len(), 4);
            for (i, s, c) in output {
                let (e, ec) = expected.stage_at_with_camera(i, (64, 36)).unwrap();
                assert_eq!(s, e.snapshot().to_render_bytes().unwrap());
                assert_eq!(c, ec.map(|c| CameraSample::capture(&c).unwrap()));
            }
        }
    }
}

#[test]
fn old_readers_and_recursive_envelopes_refuse_without_silent_audio_loss() {
    let visual = pictures(false);
    let bytes = attach_bundle_soundtrack(&visual, &track(), usize::MAX).unwrap();
    assert!(
        Reader::open(
            &bytes,
            CAMERA_TIMELINE_BUNDLE_SCHEMA,
            Limits::DEFAULT,
            UnknownPolicy::Strict
        )
        .is_err()
    );
    assert!(attach_bundle_soundtrack(&bytes, &track(), usize::MAX).is_err());
    let nested = envelope(&bytes, 8, 1, 1, &[0]);
    assert!(TimelineBundle::from_bytes(&nested).is_err());
    let mut bad = bytes.clone();
    bad[0] ^= 1;
    assert!(TimelineBundle::from_bytes(&bad).is_err());
    let mut bad = bytes.clone();
    let n = bad.len();
    bad[n - 33] ^= 1;
    assert!(TimelineBundle::from_bytes(&bad).is_err());
    assert!(
        !TimelineBundle::from_bytes(&visual)
            .unwrap()
            .has_soundtrack()
    );
}

#[test]
fn malformed_soundtracks_refuse_counts_values_layout_and_trailing_bytes() {
    let visual = pictures(true);
    for (rate, ch, n, values) in [
        (0, 1, 1, vec![0]),
        (8, 3, 1, vec![0, 0, 0]),
        (8, 2, 1, vec![0]),
        (8, 1, u64::MAX, vec![]),
        (8, 2, u64::MAX, vec![]),
        (8, 1, 0, vec![0]),
        (8, 1, 1, vec![(-0.0_f32).to_bits()]),
        (8, 1, 1, vec![f32::NAN.to_bits()]),
        (8, 1, 1, vec![f32::INFINITY.to_bits()]),
        (8, 1, 1, vec![1.01_f32.to_bits()]),
    ] {
        assert!(TimelineBundle::from_bytes(&envelope(&visual, rate, ch, n, &values)).is_err());
    }
    for prefix in [0, 4, 16, 32, 64, visual.len() - 1] {
        let bytes = envelope(&visual[..prefix], 8, 1, 1, &[0]);
        assert!(TimelineBundle::from_bytes(&bytes).is_err());
    }
}

#[test]
fn exact_complete_output_budget_and_audio_mutations_are_observable() {
    let visual = pictures(false);
    let bytes = attach_bundle_soundtrack(&visual, &track(), usize::MAX).unwrap();
    assert_eq!(
        attach_bundle_soundtrack(&visual, &track(), bytes.len()).unwrap(),
        bytes
    );
    assert!(attach_bundle_soundtrack(&visual, &track(), bytes.len() - 1).is_err());
    assert!(attach_bundle_soundtrack(&visual, &track(), 0).is_err());
    let other = BundleSoundtrack::new(11, 2, vec![0.25; 8]).unwrap();
    let changed = attach_bundle_soundtrack(&visual, &other, usize::MAX).unwrap();
    assert_ne!(sha256(&bytes), sha256(&changed));
    assert_ne!(
        TimelineBundle::from_bytes(&changed).unwrap().soundtrack(),
        Some(&track())
    );
}
