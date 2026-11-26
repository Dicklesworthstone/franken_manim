//! Camera samples follow actual post-updater captures, not guessed rig handles.

use fmn_anim::{FrameMetadata, FramePacket, RationalFrameClock};
use fmn_core::constants::{BLUE, RED};
use fmn_core::rng::RngRoot;
use fmn_hash::serial::{Limits, Reader, UnknownPolicy};
use fmn_hash::sha256;
use fmn_mobject::{Mobject, Stage};
use fmn_render::camera::CameraSample;
use fmn_render::{Camera, CameraConfig};
use fmn_scene::recording::{RecordedSceneBundle, RecordingError, SceneBundleRecorder};
use fmn_scene::timeline_bundle::{BundleReadError, TIMELINE_BUNDLE_SCHEMA, TimelineFrameCache};
use fmn_scene::{
    BundleExportLimits, CaptureReason, IntegrationError, LifecycleEvent, RuntimeConfig, Scene,
    SceneSink, TimelineBundle,
};

const RESOLUTION: (u32, u32) = (48, 32);

struct Capture {
    recorder: SceneBundleRecorder,
    observed: Vec<(Vec<u8>, CameraSample)>,
}

impl Capture {
    fn camera(&self) -> Camera {
        let n = self.observed.len() as f64;
        let mut config = CameraConfig {
            resolution: RESOLUTION,
            fps: 8,
            background: if n < 2.0 { BLUE } else { RED }.to_linear(1.0),
            light_source_position: [n + 1.0, -3.0, 7.0],
            samples: 4,
            ..CameraConfig::default()
        };
        config
            .frame
            .set_orientation([1.0, 2.3 + n, -4.7, 0.9])
            .unwrap();
        config.frame.set_center([n / 4.0, -0.25, 0.0]).unwrap();
        config.frame.set_width(5.0 - n / 4.0).unwrap();
        config.frame.set_field_of_view(0.7 + n / 16.0).unwrap();
        Camera::new(config).unwrap()
    }
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
        let camera = self.camera();
        self.observed.push((
            packet.state().to_render_bytes().unwrap(),
            CameraSample::capture(&camera).unwrap(),
        ));
        self.recorder.capture_with_camera(reason, packet, &camera)
    }

    fn capture_stage(
        &mut self,
        reason: CaptureReason,
        stage: &Stage,
        metadata: FrameMetadata,
    ) -> Result<(), IntegrationError> {
        let camera = self.camera();
        self.recorder
            .capture_stage_with_camera(reason, stage, metadata, &camera)?;
        // The preexisting full snapshot codec is the independent frame oracle.
        self.observed.push((
            stage.snapshot().to_render_bytes().unwrap(),
            CameraSample::capture(&camera).unwrap(),
        ));
        Ok(())
    }
}

fn recording() -> (RecordedSceneBundle, Vec<(Vec<u8>, CameraSample)>) {
    recording_with_storage(false)
}

fn recording_with_storage(compact: bool) -> (RecordedSceneBundle, Vec<(Vec<u8>, CameraSample)>) {
    let mut scene = Scene::new(
        RuntimeConfig {
            fps: 8,
            ..RuntimeConfig::default()
        },
        19,
    )
    .unwrap();
    let mob = scene
        .add_mobject(Mobject::from_points(&[[1.0, 2.0, 3.0]]))
        .unwrap();
    scene
        .stage_mut()
        .add_dt_updater(
            mob,
            |stage, mob, dt| {
                stage.shift(mob, [dt, 0.0, 0.0]);
            },
            false,
        )
        .unwrap();
    let mut sink = Capture {
        recorder: if compact {
            SceneBundleRecorder::new_compact_with_camera(8, Default::default()).unwrap()
        } else {
            SceneBundleRecorder::new_render_only_with_camera(8, BundleExportLimits::default())
                .unwrap()
        },
        observed: Vec::new(),
    };
    scene.wait(Some(0.5), &mut sink).unwrap();
    (sink.recorder.finish().unwrap(), sink.observed)
}

#[test]
fn compact_camera_recordings_keep_exact_samples_and_accept_final_soundtracks() {
    use fmn_scene::timeline_bundle::{BundleSegmentKind, BundleSoundtrack};

    let (artifact, observed) = recording_with_storage(true);
    assert_eq!(&artifact.bytes[10..12], &[3, 0]);
    assert_eq!(artifact.bytes, recording_with_storage(true).0.bytes);
    assert!(
        Reader::open(
            &artifact.bytes,
            fmn_scene::timeline_bundle::SOUNDTRACK_TIMELINE_BUNDLE_SCHEMA,
            Limits::DEFAULT,
            UnknownPolicy::Strict,
        )
        .is_err(),
        "old readers must refuse compact input rather than omit its changes",
    );
    let pcm = BundleSoundtrack::new(8, 1, vec![0.25, -0.5, 0.0, 1.0]).unwrap();
    let narrated = artifact
        .with_soundtrack(&pcm, Limits::DEFAULT.max_total)
        .unwrap();
    let mut bundle = TimelineBundle::from_bytes(&narrated.bytes).unwrap();
    assert!(bundle.has_soundtrack());
    assert!(bundle.has_camera_track());
    assert!(matches!(
        bundle.stage_at_with_camera(0, RESOLUTION),
        Err(BundleReadError::SoundtrackRequired),
    ));
    assert_eq!(bundle.take_soundtrack().unwrap(), pcm);
    assert_eq!(
        bundle.segment_kind(0),
        Some(BundleSegmentKind::CompactRecorded)
    );
    let shared = bundle.into_shared().unwrap();
    let mut cache = TimelineFrameCache::default();
    for index in [3, 0, 2, 1, 0] {
        let job = shared.frame_job(index).unwrap();
        assert!(matches!(
            cache.materialize(&job),
            Err(BundleReadError::CameraTrackRequired)
        ));
        let (stage, camera) = cache.materialize_with_camera(&job, RESOLUTION).unwrap();
        assert_eq!(
            stage.snapshot().to_render_bytes().unwrap(),
            observed[index as usize].0
        );
        assert_eq!(
            CameraSample::capture(&camera.unwrap()).unwrap(),
            observed[index as usize].1
        );
    }
}

#[test]
fn recorded_cameras_geometry_and_clock_survive_random_access() {
    let (artifact, observed) = recording();
    assert_eq!(artifact.frame_count, 4);
    assert_ne!(observed[0].0, observed[1].0);
    assert_ne!(observed[0].1, observed[1].1);
    assert_eq!(recording().0.bytes, artifact.bytes);
    let bundle = TimelineBundle::from_bytes(&artifact.bytes).unwrap();
    assert!(bundle.requires_camera());
    assert!(matches!(
        bundle.stage_at(0),
        Err(BundleReadError::CameraTrackRequired)
    ));
    assert!(bundle.camera_at(4, RESOLUTION).is_err());
    assert!(bundle.camera_at(0, (0, 32)).is_err());
    for index in [3, 0, 2, 1, 0] {
        let (stage, camera) = bundle.stage_at_with_camera(index, RESOLUTION).unwrap();
        let camera = camera.unwrap();
        assert_eq!(camera.revision(), u64::from(index) + 1);
        assert_eq!(
            stage.snapshot().to_render_bytes().unwrap(),
            observed[index as usize].0
        );
        assert_eq!(
            CameraSample::capture(&camera).unwrap(),
            observed[index as usize].1
        );
    }
    let shared = bundle.into_shared().unwrap();
    let mut cache = TimelineFrameCache::default();
    for index in [3, 0, 2, 1, 0] {
        let job = shared.frame_job(index).unwrap();
        assert!(matches!(
            job.materialize(),
            Err(BundleReadError::CameraTrackRequired)
        ));
        let (stage, camera) = cache.materialize_with_camera(&job, RESOLUTION).unwrap();
        assert_eq!(
            stage.snapshot().to_render_bytes().unwrap(),
            observed[index as usize].0
        );
        assert_eq!(
            CameraSample::capture(&camera.unwrap()).unwrap(),
            observed[index as usize].1
        );
    }
}

fn rehash(bytes: &mut [u8]) {
    let end = bytes.len() - 32;
    let digest = sha256(&bytes[..end]);
    bytes[end..].copy_from_slice(digest.as_bytes());
}

#[test]
fn old_readers_and_malformed_camera_tables_refuse() {
    let (artifact, _) = recording();
    assert_eq!(&artifact.bytes[10..12], &1u16.to_le_bytes());
    assert!(
        Reader::open(
            &artifact.bytes,
            TIMELINE_BUNDLE_SCHEMA,
            Limits::DEFAULT,
            UnknownPolicy::Strict
        )
        .is_err()
    );
    let start = artifact.bytes.len() - 32 - 4 * CameraSample::WIRE_BYTES;
    for (offset, replacement) in [
        (start - 4, 3u32.to_le_bytes().to_vec()),
        (start - 4, u32::MAX.to_le_bytes().to_vec()),
        (start, 0u32.to_le_bytes().to_vec()),
        (start + 48, vec![0; 32]),
        (start + 80, 0f64.to_le_bytes().to_vec()),
    ] {
        let mut bytes = artifact.bytes.clone();
        bytes[offset..offset + replacement.len()].copy_from_slice(&replacement);
        rehash(&mut bytes);
        assert!(TimelineBundle::from_bytes(&bytes).is_err());
    }
    let mut newer = artifact.bytes.clone();
    newer[10..12].copy_from_slice(&2u16.to_le_bytes());
    rehash(&mut newer);
    assert!(TimelineBundle::from_bytes(&newer).is_err());
}

#[test]
fn mode_mismatches_missing_samples_and_capture_budgets_are_sticky() {
    let stage = Stage::new();
    let camera = Camera::new(CameraConfig {
        fps: 8,
        ..CameraConfig::default()
    })
    .unwrap();
    let mut missing =
        SceneBundleRecorder::new_render_only_with_camera(8, BundleExportLimits::default()).unwrap();
    assert!(missing.capture_terminal_still(&stage).is_err());
    assert!(
        missing
            .capture_terminal_still_with_camera(&stage, &camera)
            .is_err()
    );
    assert!(missing.finish().is_err());
    let mut ordinary = SceneBundleRecorder::new(8, BundleExportLimits::default()).unwrap();
    assert!(
        ordinary
            .capture_terminal_still_with_camera(&stage, &camera)
            .is_err()
    );
    assert!(ordinary.finish().is_err());
    let mut wrong_fps =
        SceneBundleRecorder::new_render_only_with_camera(30, BundleExportLimits::default())
            .unwrap();
    assert!(
        wrong_fps
            .capture_terminal_still_with_camera(&stage, &camera)
            .is_err()
    );
    assert!(wrong_fps.finish().is_err());
    let mut limited = SceneBundleRecorder::new_render_only_with_camera(
        8,
        BundleExportLimits {
            max_frames: 1,
            max_capture_bytes: CameraSample::WIRE_BYTES,
        },
    )
    .unwrap();
    assert!(
        limited
            .capture_terminal_still_with_camera(&stage, &camera)
            .is_err()
    );
    assert!(limited.finish().is_err());
}

#[test]
fn terminal_stills_and_exact_output_caps_preserve_legacy_writers() {
    let stage = Stage::new();
    let camera = Camera::new(CameraConfig {
        fps: 8,
        ..CameraConfig::default()
    })
    .unwrap();
    let legacy = || {
        let mut recorder =
            SceneBundleRecorder::new_render_only(8, BundleExportLimits::default()).unwrap();
        recorder.capture_terminal_still(&stage).unwrap();
        recorder.finish().unwrap()
    };
    let old = legacy();
    assert_eq!(old.bytes, legacy().bytes);
    assert_eq!(&old.bytes[10..12], &[0, 0]);
    let decoded = TimelineBundle::from_bytes(&old.bytes).unwrap();
    assert!(!decoded.has_camera_track());
    assert!(
        decoded
            .stage_at_with_camera(0, RESOLUTION)
            .unwrap()
            .1
            .is_none()
    );
    let build = |limit| {
        let mut recorder =
            SceneBundleRecorder::new_render_only_with_camera(8, BundleExportLimits::default())
                .unwrap();
        recorder
            .capture_terminal_still_with_camera(&stage, &camera)
            .unwrap();
        recorder.finish_with_max_bytes(limit)
    };
    let artifact = build(Limits::DEFAULT.max_total).unwrap();
    assert_eq!(build(artifact.bytes.len()).unwrap().bytes, artifact.bytes);
    assert!(matches!(
        build(artifact.bytes.len() - 1),
        Err(RecordingError::OutputLimit { .. })
    ));
}

#[test]
fn direct_camera_capture_matches_packet_admission_and_sticky_refusals() {
    let stage = Stage::new();
    let rng = RngRoot::from_seed(19);
    for refusal in 0..5 {
        let clock = RationalFrameClock::new(if refusal == 0 { 30 } else { 8 }).unwrap();
        let metadata = FrameMetadata::capture_barrier(&clock, &rng);
        let camera = Camera::new(CameraConfig {
            fps: if refusal == 1 { 30 } else { 8 },
            ..CameraConfig::default()
        })
        .unwrap();
        let limits = BundleExportLimits {
            max_frames: if refusal == 3 { 0 } else { 8 },
            max_capture_bytes: if refusal == 4 {
                0
            } else {
                BundleExportLimits::default().max_capture_bytes
            },
        };
        let make = || {
            if refusal == 2 {
                SceneBundleRecorder::new_render_only(8, limits).unwrap()
            } else {
                SceneBundleRecorder::new_render_only_with_camera(8, limits).unwrap()
            }
        };
        let mut packets = make();
        let mut direct = make();
        let old = packets
            .capture_with_camera(
                CaptureReason::Show,
                FramePacket::freeze_metadata(&stage, metadata),
                &camera,
            )
            .unwrap_err();
        let new = direct
            .capture_stage_with_camera(CaptureReason::Show, &stage, metadata, &camera)
            .unwrap_err();
        assert_eq!(new.to_string(), old.to_string());
        assert_eq!(
            direct
                .capture_stage_with_camera(CaptureReason::Show, &stage, metadata, &camera)
                .unwrap_err()
                .to_string(),
            old.to_string()
        );
        assert_eq!(direct.frame_count(), 0);
        assert_eq!(
            direct.finish().unwrap_err().to_string(),
            packets.finish().unwrap_err().to_string()
        );
    }
}
