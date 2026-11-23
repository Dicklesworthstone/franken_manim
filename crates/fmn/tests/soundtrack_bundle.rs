//! Native scenes compile into portable audible recordings and replay without assets.

#[cfg(all(
    any(target_os = "linux", target_os = "macos"),
    any(target_arch = "x86_64", target_arch = "aarch64")
))]
#[path = "support/compressed_audio.rs"]
mod compressed_audio;
#[allow(dead_code)]
#[path = "support/surround_audio.rs"]
mod surround_audio;

use fmn::exporting::soundtrack::*;
use fmn::exporting::{
    BundleExportError, BundleExportOptions, export_bundle_bytes, export_camera_bundle_bytes,
};
use fmn::prelude::*;
use fmn::rendering::{SoundtrackPreparationLimits, render_bundle_with_fs};
use fmn_codec::{SampleFormat, WavAudio, encode_wav};
use fmn_output::{
    AudioDecodeError, AudioDecodeLimits, AudioDecoder, AudioInputFormat, MixerConfig, SoundCue,
    SoundMixer,
};
use fmn_platform::fs::{FileSystem, FsError, FsNodeKind, StdFs, VirtualFs};
use fmn_platform::process::{FfmpegLocator, StdFfmpegLocator, StdProcessRunner};
use fmn_scene::{CameraRig, TimelineBundle};
use std::{cell::Cell, path::Path, rc::Rc, sync::Arc};

fn background() -> WavAudio {
    WavAudio {
        channels: 1,
        channel_mask: None,
        sample_rate: 8,
        format: SampleFormat::F32,
        samples: vec![0.125, 0.25, 0.375, 0.5, 0.25, 0.0, -0.125, -0.25],
    }
}
fn lead() -> WavAudio {
    WavAudio {
        channels: 2,
        channel_mask: None,
        sample_rate: 11,
        format: SampleFormat::F32,
        samples: vec![0.25, -0.25, 0.5, -0.5, 0.75, -0.75, 0.25, -0.25],
    }
}
fn assets() -> Arc<VirtualFs> {
    let fs = Arc::new(VirtualFs::new());
    for (path, pcm) in [("/background.wav", background()), ("/lead.wav", lead())] {
        fs.insert(
            path,
            encode_wav(
                pcm.channels,
                pcm.sample_rate,
                SampleFormat::F32,
                &pcm.samples,
            ),
        );
    }
    fs
}
fn options() -> BundleExportOptions {
    let mut o = BundleExportOptions::new().unwrap();
    o.config.camera.fps = 8;
    o.config.camera.resolution = (64, 36);
    o
}
fn audio(workers: usize, rate: u32) -> BundleSoundtrackOptions {
    BundleSoundtrackOptions {
        mixer: MixerConfig {
            sample_rate: rate,
            channels: 2,
            max_output_frames: 100_000,
        },
        workers,
        ..BundleSoundtrackOptions::default()
    }
}
struct Explanation {
    sound: bool,
    calls: Rc<Cell<usize>>,
}
impl Explanation {
    fn new(sound: bool) -> Self {
        Self {
            sound,
            calls: Rc::new(Cell::new(0)),
        }
    }
}

#[test]
fn surround_file_and_memory_sources_export_identical_portable_stereo_pcm() {
    assert_eq!(surround_audio::verify_bundle().unwrap(), 4);
}
impl SceneConstruct for Explanation {
    fn construct(&mut self, stage: &mut Stage<'_>) -> fmn::Result<()> {
        self.calls.set(self.calls.get() + 1);
        let mob = stage.add(Circle::new().radius(0.5).color(BLUE))?;
        stage.scene_mut().stage_mut().add_dt_updater(
            mob,
            |s, m, dt| {
                s.shift(m, [dt, 0.0, 0.0]);
            },
            false,
        )?;
        if self.sound {
            stage
                .scene_mut()
                .add_sound("/background.wav", 0.0, None, None)?;
        }
        stage.wait(0.25)?;
        if self.sound {
            stage
                .scene_mut()
                .add_sound("/lead.wav", -0.125, Some(-6.0), Some(-3.0))?;
        }
        stage.wait(0.25)?;
        Ok(())
    }
}
fn expected(rate: u32) -> Vec<f32> {
    let mut mixer = SoundMixer::new(audio(1, rate).mixer).unwrap();
    mixer.add(SoundCue::new(background(), 0, 8)).unwrap();
    mixer
        .add(SoundCue {
            audio: lead(),
            frame: 2,
            fps: 8,
            time_offset: -0.125,
            gain: Some(-6.0),
            gain_to_background: Some(-3.0),
        })
        .unwrap();
    mixer
        .with_timeline_frames(u64::from(rate) / 2)
        .mix(1)
        .unwrap()
        .audio
        .samples
}
#[test]
fn actual_scene_audio_matches_native_mix_and_original_capture_at_all_worker_counts() {
    let fs = assets();
    let silent = export_bundle_bytes(&mut Explanation::new(false), options()).unwrap();
    let visual = TimelineBundle::from_bytes(&silent.bundle.bytes).unwrap();
    let mut reference = None;
    for workers in [1, 4, 16] {
        let mut program = Explanation::new(true);
        let export = export_bundle_with_soundtrack_bytes(
            &mut program,
            options(),
            &audio(workers, 8),
            fs.as_ref(),
        )
        .unwrap();
        assert_eq!(program.calls.get(), 1);
        assert_eq!(export.scene.time.frames(), 4);
        assert_eq!(export.bundle.frame_count, 4);
        assert_eq!(export.bundle.segment_count, 2);
        let mut decoded = TimelineBundle::from_bytes(&export.bundle.bytes).unwrap();
        let track = decoded.take_soundtrack().unwrap();
        assert_eq!(
            (track.sample_rate(), track.channels(), track.sample_frames()),
            (8, 2, 8)
        );
        assert_eq!(track.samples(), expected(8));
        for i in [3, 0, 1, 2] {
            assert_eq!(
                decoded
                    .stage_at(i)
                    .unwrap()
                    .snapshot()
                    .to_render_bytes()
                    .unwrap(),
                visual
                    .stage_at(i)
                    .unwrap()
                    .snapshot()
                    .to_render_bytes()
                    .unwrap()
            );
        }
        assert!(
            !export
                .bundle
                .bytes
                .windows(b"/background.wav".len())
                .any(|b| b == b"/background.wav")
        );
        if let Some(ref bytes) = reference {
            assert_eq!(&export.bundle.bytes, bytes);
        } else {
            reference = Some(export.bundle.bytes);
        }
    }
}
#[test]
fn camera_export_keeps_audio_and_camera_in_the_same_recording() {
    let fs = assets();
    let mut base = fmn_render::CameraConfig {
        fps: 8,
        resolution: (64, 36),
        ..Default::default()
    };
    base.frame.set_center([1.0, 0.5, 0.0]).unwrap();
    let factory = |sound| {
        move |scene: &mut fmn_scene::Scene, config: &fmn_render::CameraConfig| {
            Ok((Explanation::new(sound), CameraRig::new(scene, config)?))
        }
    };
    let legacy = export_camera_bundle_bytes(factory(false), base.clone(), options()).unwrap();
    let export = export_camera_bundle_with_soundtrack_bytes(
        factory(true),
        base,
        options(),
        &audio(4, 8),
        fs.as_ref(),
    )
    .unwrap();
    let mut decoded = TimelineBundle::from_bytes(&export.bundle.bytes).unwrap();
    assert!(decoded.has_camera_track());
    assert_eq!(decoded.take_soundtrack().unwrap().samples(), expected(8));
    let old = TimelineBundle::from_bytes(&legacy.bundle.bytes).unwrap();
    for i in [3, 0, 2, 1] {
        let (a, ac) = decoded.stage_at_with_camera(i, (64, 36)).unwrap();
        let (b, bc) = old.stage_at_with_camera(i, (64, 36)).unwrap();
        assert_eq!(
            a.snapshot().to_render_bytes().unwrap(),
            b.snapshot().to_render_bytes().unwrap()
        );
        assert_eq!(
            fmn_render::camera::CameraSample::capture(&ac.unwrap()).unwrap(),
            fmn_render::camera::CameraSample::capture(&bc.unwrap()).unwrap()
        );
    }
}
#[test]
fn no_cues_preserve_legacy_bytes_and_bad_configuration_precedes_scene_execution() {
    let plain = export_bundle_bytes(&mut Explanation::new(false), options()).unwrap();
    let enabled = export_bundle_with_soundtrack_bytes(
        &mut Explanation::new(false),
        options(),
        &audio(1, 8),
        &VirtualFs::new(),
    )
    .unwrap();
    assert_eq!(plain.bundle.bytes, enabled.bundle.bytes);
    assert!(plain.soundtrack.is_none());
    assert!(enabled.soundtrack.is_none());
    for bad in [
        BundleSoundtrackOptions {
            workers: 0,
            ..audio(1, 8)
        },
        BundleSoundtrackOptions {
            mixer: MixerConfig {
                channels: 3,
                ..audio(1, 8).mixer
            },
            ..audio(1, 8)
        },
        BundleSoundtrackOptions {
            preparation: SoundtrackPreparationLimits {
                max_cues: 0,
                ..Default::default()
            },
            ..audio(1, 8)
        },
    ] {
        let mut program = Explanation::new(true);
        assert!(
            export_bundle_with_soundtrack_bytes(&mut program, options(), &bad, assets().as_ref())
                .is_err()
        );
        assert_eq!(program.calls.get(), 0);
    }
    assert!(export_bundle_bytes(&mut Explanation::new(true), options()).is_err());
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ShowAt {
    BeforeCue,
    AfterCue,
    AfterWait,
}

struct ShownExplanation {
    sound: bool,
    at: ShowAt,
}

impl SceneConstruct for ShownExplanation {
    fn construct(&mut self, stage: &mut Stage<'_>) -> fmn::Result<()> {
        stage.add(Circle::new().radius(0.5))?;
        if self.at == ShowAt::BeforeCue {
            stage.show()?;
        }
        if self.sound {
            stage.scene_mut().add_sound("/tone.wav", 0.0, None, None)?;
        }
        if self.at == ShowAt::AfterCue {
            stage.show()?;
        }
        stage.wait(0.25)?;
        if self.at == ShowAt::AfterWait {
            stage.show()?;
        }
        stage.wait(0.25)?;
        Ok(())
    }
}

/// Observe the actual export capability boundary, independently of its result.
/// Even probing an asset or creating a private publication file is too early.
struct ObservedExportFs {
    inner: Arc<VirtualFs>,
    accesses: std::sync::atomic::AtomicUsize,
}

impl ObservedExportFs {
    fn observed(&self) -> &VirtualFs {
        self.accesses
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        self.inner.as_ref()
    }
}

impl FileSystem for ObservedExportFs {
    fn node_kind_no_follow(&self, path: &Path) -> Result<Option<FsNodeKind>, FsError> {
        self.observed().node_kind_no_follow(path)
    }
    fn create_dir(&self, path: &Path) -> Result<bool, FsError> {
        self.observed().create_dir(path)
    }
    fn read(&self, path: &Path) -> Result<Vec<u8>, FsError> {
        self.observed().read(path)
    }
    fn read_bounded(&self, path: &Path, max_bytes: usize) -> Result<Vec<u8>, FsError> {
        self.observed().read_bounded(path, max_bytes)
    }
    fn write_atomic(&self, path: &Path, bytes: &[u8]) -> Result<(), FsError> {
        self.observed().write_atomic(path, bytes)
    }
    fn begin_atomic_file(
        self: Arc<Self>,
        path: &Path,
    ) -> Result<Box<dyn fmn_platform::fs::AtomicFileWriter>, FsError> {
        self.observed();
        Arc::clone(&self.inner).begin_atomic_file(path)
    }
    fn create_new(&self, path: &Path, bytes: &[u8]) -> Result<bool, FsError> {
        self.observed().create_new(path, bytes)
    }
    fn remove_file(&self, path: &Path) -> Result<(), FsError> {
        self.observed().remove_file(path)
    }
    fn remove_dir_all(&self, path: &Path) -> Result<(), FsError> {
        self.observed().remove_dir_all(path)
    }
    fn exists(&self, path: &Path) -> bool {
        self.observed().exists(path)
    }
    fn list_dir(&self, path: &Path) -> Result<Vec<std::path::PathBuf>, FsError> {
        self.observed().list_dir(path)
    }
    fn count_dir_entries_bounded(&self, path: &Path, max_entries: usize) -> Result<usize, FsError> {
        self.observed().count_dir_entries_bounded(path, max_entries)
    }
}

#[test]
fn explicit_show_with_sound_refuses_before_assets_audio_budgets_or_publication() {
    for compact in [false, true] {
        for camera in [false, true] {
            for at in [ShowAt::BeforeCue, ShowAt::AfterCue, ShowAt::AfterWait] {
                for constrained in [false, true] {
                    let inner = Arc::new(VirtualFs::new());
                    inner.insert("/existing.fmtl", b"keep existing artifact".to_vec());
                    if !constrained {
                        inner.insert("/tone.wav", encode_wav(1, 8, SampleFormat::F32, &[0.25]));
                    }
                    let fs = Arc::new(ObservedExportFs {
                        inner: Arc::clone(&inner),
                        accesses: std::sync::atomic::AtomicUsize::new(0),
                    });
                    let mut opts = options();
                    if compact {
                        opts.storage = fmn::exporting::BundleStorage::Compact(Default::default());
                    }
                    let (mut settings, decoder_calls) = unavailable_decoder();
                    if constrained {
                        // A missing source and exhausted audio budgets must
                        // not replace the earlier semantic refusal.
                        settings.preparation.max_source_bytes = 1;
                        settings.preparation.max_prepared_samples = 1;
                    }
                    let destination = if constrained {
                        "/existing.fmtl"
                    } else {
                        "/new.fmtl"
                    };
                    let mut program = ShownExplanation { sound: true, at };
                    let error = if camera {
                        export_camera_bundle_with_soundtrack_with_fs(
                            move |scene, config| Ok((program, CameraRig::new(scene, config)?)),
                            destination,
                            fmn_render::CameraConfig {
                                fps: 8,
                                resolution: (64, 36),
                                ..Default::default()
                            },
                            opts,
                            &settings,
                            fs.clone(),
                        )
                    } else {
                        export_bundle_with_soundtrack_with_fs(
                            &mut program,
                            destination,
                            opts,
                            &settings,
                            fs.clone(),
                        )
                    }
                    .unwrap_err();
                    assert!(
                        matches!(error, BundleExportError::Capability(message)
                            if message.contains("explicit show") && message.contains("play/wait")),
                        "compact={compact}, camera={camera}, at={at:?}: {error}"
                    );
                    assert_eq!(fs.accesses.load(std::sync::atomic::Ordering::SeqCst), 0);
                    assert_eq!(decoder_calls.load(std::sync::atomic::Ordering::SeqCst), 0);
                    assert!(!inner.exists(Path::new("/new.fmtl")));
                    assert_eq!(
                        inner.read(Path::new("/existing.fmtl")).unwrap(),
                        b"keep existing artifact"
                    );
                }
            }
        }
    }
}

#[test]
fn show_audio_refusal_preserves_recording_and_output_failures() {
    struct CaughtCaptureFailure;
    impl SceneConstruct for CaughtCaptureFailure {
        fn construct(&mut self, stage: &mut Stage<'_>) -> fmn::Result<()> {
            stage.show()?;
            stage
                .scene_mut()
                .add_sound("/missing.wav", 0.0, None, None)?;
            let _ = stage.wait(0.25);
            Ok(())
        }
    }

    for compact in [false, true] {
        for camera in [false, true] {
            for capture_failure in [false, true] {
                let mut opts = options();
                if compact {
                    let mut limits = fmn::exporting::CompactRecordingLimits::default();
                    if capture_failure {
                        limits.max_frames = 1;
                    }
                    opts.storage = fmn::exporting::BundleStorage::Compact(limits);
                } else if capture_failure {
                    opts.limits.max_frames = 1;
                }
                if !capture_failure {
                    opts.max_output_bytes = 1;
                }
                let fs = ObservedExportFs {
                    inner: Arc::new(VirtualFs::new()),
                    accesses: std::sync::atomic::AtomicUsize::new(0),
                };
                let error = if camera {
                    export_camera_bundle_with_soundtrack_bytes(
                        |scene, config| Ok((CaughtCaptureFailure, CameraRig::new(scene, config)?)),
                        fmn_render::CameraConfig {
                            fps: 8,
                            resolution: (64, 36),
                            ..Default::default()
                        },
                        opts,
                        &audio(1, 8),
                        &fs,
                    )
                } else {
                    export_bundle_with_soundtrack_bytes(
                        &mut CaughtCaptureFailure,
                        opts,
                        &audio(1, 8),
                        &fs,
                    )
                }
                .unwrap_err();
                if capture_failure {
                    assert!(
                        matches!(
                            error,
                            BundleExportError::Recording(
                                fmn_scene::recording::RecordingError::Bundle(
                                    fmn_scene::timeline_bundle::BundleError::FrameLimitExceeded {
                                        frames: 2,
                                        max_frames: 1,
                                    }
                                )
                            )
                        ),
                        "{error}"
                    );
                } else {
                    assert!(
                        matches!(error, BundleExportError::OutputLimit { limit: 1, .. }),
                        "{error}"
                    );
                }
                assert_eq!(fs.accesses.load(std::sync::atomic::Ordering::SeqCst), 0);
            }
        }
    }
}

#[test]
fn explicit_show_without_sound_preserves_the_recorded_grid_with_audio_enabled() {
    for compact in [false, true] {
        for camera in [false, true] {
            let mut opts = options();
            if compact {
                opts.storage = fmn::exporting::BundleStorage::Compact(Default::default());
            }
            let program = || ShownExplanation {
                sound: false,
                at: ShowAt::BeforeCue,
            };
            let fs = VirtualFs::new();
            let settings = audio(1, 8);
            let (legacy, enabled) = if camera {
                let config = fmn_render::CameraConfig {
                    fps: 8,
                    resolution: (64, 36),
                    ..Default::default()
                };
                (
                    export_camera_bundle_bytes(
                        |scene, config| Ok((program(), CameraRig::new(scene, config)?)),
                        config.clone(),
                        opts.clone(),
                    ),
                    export_camera_bundle_with_soundtrack_bytes(
                        |scene, config| Ok((program(), CameraRig::new(scene, config)?)),
                        config,
                        opts,
                        &settings,
                        &fs,
                    ),
                )
            } else {
                (
                    export_bundle_bytes(&mut program(), opts.clone()),
                    export_bundle_with_soundtrack_bytes(&mut program(), opts, &settings, &fs),
                )
            };
            let (legacy, enabled) = (legacy.unwrap(), enabled.unwrap());
            assert_eq!(enabled.bundle.bytes, legacy.bundle.bytes);
            assert!(enabled.soundtrack.is_none());
            assert_eq!(enabled.bundle.frame_count, 5);
            assert_eq!(enabled.bundle.segment_count, 3);
            assert_eq!(enabled.scene.time.frames(), 4);
        }
    }
}

#[test]
fn cumulative_audio_limits_and_missing_assets_never_publish_partial_or_overwrite() {
    for field in 0..5 {
        let fs = assets();
        fs.insert("/existing.fmtl", b"keep me".to_vec());
        let mut settings = audio(1, 8);
        match field {
            0 => settings.preparation.max_cues = 1,
            1 => settings.preparation.max_source_bytes = 1,
            2 => settings.preparation.max_decoded_samples = 7,
            3 => settings.preparation.max_prepared_samples = 15,
            _ => settings.mixer.max_output_frames = 3,
        }
        let error = export_bundle_with_soundtrack_with_fs(
            &mut Explanation::new(true),
            "/new.fmtl",
            options(),
            &settings,
            fs.clone(),
        );
        assert!(error.is_err(), "field {field}");
        assert!(fs.read(Path::new("/new.fmtl")).is_err());
        assert_eq!(fs.read(Path::new("/existing.fmtl")).unwrap(), b"keep me");
    }
    let fs = assets();
    fs.remove_file(Path::new("/lead.wav")).unwrap();
    assert!(
        export_bundle_with_soundtrack_with_fs(
            &mut Explanation::new(true),
            "/missing.fmtl",
            options(),
            &audio(1, 8),
            fs.clone()
        )
        .is_err()
    );
    assert!(fs.read(Path::new("/missing.fmtl")).is_err());
    let fs = assets();
    let first = export_bundle_with_soundtrack_with_fs(
        &mut Explanation::new(true),
        "/complete.fmtl",
        options(),
        &audio(1, 8),
        fs.clone(),
    )
    .unwrap();
    let bytes = fs.read(Path::new("/complete.fmtl")).unwrap();
    assert_eq!(first.bytes, bytes.len());
    assert!(
        export_bundle_with_soundtrack_with_fs(
            &mut Explanation::new(true),
            "/complete.fmtl",
            options(),
            &audio(1, 8),
            fs.clone()
        )
        .is_err()
    );
    assert_eq!(fs.read(Path::new("/complete.fmtl")).unwrap(), bytes);
    let mut capped = options();
    capped.max_output_bytes = bytes.len() - 1;
    assert!(
        export_bundle_with_soundtrack_bytes(
            &mut Explanation::new(true),
            capped,
            &audio(1, 8),
            fs.as_ref()
        )
        .is_err()
    );
}
#[test]
fn picture_only_replay_refuses_audio_before_publishing() {
    let source = export_bundle_with_soundtrack_bytes(
        &mut Explanation::new(true),
        options(),
        &audio(1, 8),
        assets().as_ref(),
    )
    .unwrap();
    for format in [
        RenderFormat::PngSequence,
        RenderFormat::Gif,
        RenderFormat::Y4m,
    ] {
        let fs = Arc::new(VirtualFs::new());
        let mut o = RenderOptions::with_format("/forbidden", format).unwrap();
        o.config.camera.fps = 8;
        o.config.camera.resolution = (64, 36);
        let error = render_bundle_with_fs(&source.bundle.bytes, o, fs.clone()).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("sound-bearing FMTL requires MP4/MOV")
        );
        assert!(fs.read(Path::new("/forbidden")).is_err());
    }
}

fn contains_track(bytes: &[u8], target: &[u8; 4]) -> bool {
    fn scan(bytes: &[u8], target: &[u8; 4], depth: u8) -> bool {
        if depth > 4 {
            return false;
        }
        let mut at = 0;
        while at + 8 <= bytes.len() {
            let size = u32::from_be_bytes(bytes[at..at + 4].try_into().unwrap()) as usize;
            if size < 8 || size > bytes.len() - at {
                return false;
            }
            let kind = &bytes[at + 4..at + 8];
            let body = &bytes[at + 8..at + size];
            if kind == b"hdlr" && body.len() >= 12 && &body[8..12] == target {
                return true;
            }
            if [b"moov", b"trak", b"mdia"].contains(&kind.try_into().unwrap())
                && scan(body, target, depth + 1)
            {
                return true;
            }
            at += size;
        }
        false
    }
    scan(bytes, target, 0)
}
#[test]
fn native_video_replays_stored_sound_after_all_source_assets_are_gone() {
    let locator = StdFfmpegLocator::from_host_path();
    if locator.locate_ffmpeg(Path::new("ffmpeg")).is_err() {
        fmn_core::test_inputs::skip_or_fail(
            "native_video_replays_stored_sound_after_all_source_assets_are_gone",
            "ffmpeg on PATH",
        );
        return;
    }
    let fs = assets();
    let source = export_bundle_with_soundtrack_bytes(
        &mut Explanation::new(true),
        options(),
        &audio(4, 48_000),
        fs.as_ref(),
    )
    .unwrap();
    fs.remove_file(Path::new("/background.wav")).unwrap();
    fs.remove_file(Path::new("/lead.wav")).unwrap();
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let root =
        std::env::temp_dir().join(format!("fmn-bundle-audio-{}-{stamp}", std::process::id()));
    let directory = std::fs::DirBuilder::new();
    #[cfg(unix)]
    let directory = {
        use std::os::unix::fs::DirBuilderExt;
        let mut directory = directory;
        directory.mode(0o700);
        directory
    };
    directory.create(&root).unwrap();
    let root = root.canonicalize().unwrap();
    let capability = FfmpegCapability {
        runner: Arc::new(StdProcessRunner),
        locator: Arc::new(locator),
        workdir_root: root.clone(),
    };
    for (format, name, workers, slots) in [
        (RenderFormat::Mp4, "one.mp4", 1, 1),
        (RenderFormat::Mp4, "four.mp4", 4, 2),
        (RenderFormat::Mov, "sixteen.mov", 16, 4),
    ] {
        let output = root.join(name);
        let mut o = RenderOptions::with_format(&output, format).unwrap();
        o.config.camera.fps = 8;
        o.config.camera.resolution = (64, 36);
        o.config.render.threads = fmn_config::config::ThreadPolicy::Fixed(workers);
        o.frames_in_flight = slots;
        o.ffmpeg = Some(capability.clone());
        o.ffmpeg_limits.keep_workdir = true;
        let report = render_bundle_with_fs(&source.bundle.bytes, o, Arc::new(StdFs)).unwrap();
        assert_eq!(report.artifact.frame_count, 4);
        assert_eq!(report.artifact.ffmpeg.len(), 2);
        assert!(report.artifact.audio_decodes.is_empty());
        let stats = report.frame_pipeline.unwrap();
        assert_eq!(stats.outstanding_slots, 0);
        assert!(stats.max_in_flight <= slots);
        let sound = report.artifact.soundtrack.unwrap();
        assert_eq!(
            (sound.sample_rate, sound.channels, sound.sample_frames),
            (48_000, 2, 48_000)
        );
        let argv = &report.artifact.ffmpeg[1].provenance.argv;
        let inputs: Vec<_> = argv
            .windows(2)
            .filter(|v| v[0] == "-i")
            .map(|v| &v[1])
            .collect();
        assert_eq!(inputs.len(), 2);
        let wav = std::fs::read(inputs[1]).unwrap();
        let decoded = AudioDecoder::new(AudioDecodeLimits::default(), None)
            .unwrap()
            .decode(&wav)
            .unwrap();
        let expected = expected(48_000);
        assert_eq!(decoded.audio.samples.len(), expected.len());
        // Output may choose S16 for the encoder input; only its explicit PCM
        // quantization tolerance is permitted, never a shifted or missing cue.
        for (a, b) in decoded.audio.samples.iter().zip(&expected) {
            assert!((a - b).abs() <= 1.0 / 32768.0);
        }
        let bytes = std::fs::read(&output).unwrap();
        assert!(contains_track(&bytes, b"vide"));
        assert!(contains_track(&bytes, b"soun"));
    }
}

#[test]
fn short_cues_pad_to_picture_and_terminal_stills_without_advancing_authored_time() {
    struct Short {
        hold: bool,
    }
    impl SceneConstruct for Short {
        fn construct(&mut self, stage: &mut Stage<'_>) -> fmn::Result<()> {
            stage.add(Circle::new().radius(0.5))?;
            stage.scene_mut().add_sound("/short.wav", 0.0, None, None)?;
            if self.hold {
                stage.wait(0.5)?;
            }
            Ok(())
        }
    }
    let fs = VirtualFs::new();
    fs.insert("/short.wav", encode_wav(1, 8, SampleFormat::F32, &[0.25]));
    for compact in [false, true] {
        for camera in [false, true] {
            for (hold, pictures, expected) in [
                (true, 4, vec![0.25, 0.25, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0]),
                (false, 1, vec![0.25, 0.25]),
            ] {
                let mut opts = options();
                if compact {
                    opts.storage = fmn::exporting::BundleStorage::Compact(Default::default());
                }
                let export = if camera {
                    export_camera_bundle_with_soundtrack_bytes(
                        |scene, config| Ok((Short { hold }, CameraRig::new(scene, config)?)),
                        fmn_render::CameraConfig {
                            fps: 8,
                            resolution: (64, 36),
                            ..Default::default()
                        },
                        opts,
                        &audio(1, 8),
                        &fs,
                    )
                } else {
                    export_bundle_with_soundtrack_bytes(
                        &mut Short { hold },
                        opts,
                        &audio(1, 8),
                        &fs,
                    )
                }
                .unwrap();
                assert_eq!(export.bundle.frame_count, pictures);
                assert_eq!(export.scene.time.frames(), if hold { 4 } else { 0 });
                let bundle = TimelineBundle::from_bytes(&export.bundle.bytes).unwrap();
                assert_eq!(bundle.has_camera_track(), camera);
                assert_eq!(bundle.soundtrack().unwrap().samples(), expected);
            }
        }
    }
}

#[test]
fn repeated_sources_are_charged_once_but_prepared_cues_each_consume_their_own_budget() {
    use fmn::rendering::PreparedSceneSoundtrack;
    use fmn_anim::RationalFrameClock;
    use fmn_scene::SoundRequest;
    let fs = VirtualFs::new();
    let source = encode_wav(1, 8, SampleFormat::F32, &[0.125, 0.25]);
    fs.insert("/repeat.wav", source.clone());
    let request = |frame| {
        let mut clock = RationalFrameClock::new(8).unwrap();
        clock.advance_frames(frame).unwrap();
        SoundRequest {
            source: fmn_scene::SoundSource::File("/repeat.wav".into()),
            time: clock.now(),
            time_offset: 0.0,
            absolute_time: None,
            gain: None,
            gain_to_background: None,
        }
    };
    let mut limits = SoundtrackPreparationLimits {
        max_cues: 2,
        max_source_bytes: source.len(),
        max_decoded_samples: 2,
        max_prepared_samples: 8,
    };
    let prepare = |limits| {
        PreparedSceneSoundtrack::new_with_limits(
            &fs,
            &[request(0), request(3)],
            &mut AudioDecoder::new(AudioDecodeLimits::default(), None).unwrap(),
            audio(1, 8).mixer,
            None,
            limits,
        )
    };
    let prepared = prepare(limits).unwrap();
    assert!(Arc::ptr_eq(
        &prepared.inputs()[0].bytes,
        &prepared.inputs()[1].bytes
    ));
    assert_eq!(
        prepared.mix(4).unwrap().audio.samples,
        [0.125, 0.125, 0.25, 0.25, 0.0, 0.0, 0.125, 0.125, 0.25, 0.25]
    );
    limits.max_prepared_samples = 7;
    assert!(
        prepare(limits)
            .err()
            .unwrap()
            .to_string()
            .contains("prepared-sample budget")
    );
}

struct UnavailableDecoder {
    calls: Arc<std::sync::atomic::AtomicUsize>,
}

impl FfmpegLocator for UnavailableDecoder {
    fn locate_ffmpeg(
        &self,
        _: &Path,
    ) -> Result<fmn_platform::process::FfmpegExecutable, fmn_platform::process::FfmpegLocatorError>
    {
        self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Err(fmn_platform::process::FfmpegLocatorError::NotFound)
    }
}

fn unavailable_decoder() -> (BundleSoundtrackOptions, Arc<std::sync::atomic::AtomicUsize>) {
    let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let mut settings = audio(1, 8);
    settings.ffmpeg = Some(FfmpegCapability {
        runner: Arc::new(fmn_platform::process::NoProcessRunner),
        locator: Arc::new(UnavailableDecoder {
            calls: Arc::clone(&calls),
        }),
        workdir_root: "/unused-bundle-audio".into(),
    });
    (settings, calls)
}

#[test]
fn native_and_no_cue_exports_never_discover_the_offered_decoder() {
    let fs = assets();
    let (settings, calls) = unavailable_decoder();
    for mode in [DeterminismMode::Standard, DeterminismMode::Certified] {
        let mut opts = options();
        opts.config.determinism.mode = mode;
        let plain = export_bundle_bytes(&mut Explanation::new(false), opts.clone()).unwrap();
        let silent = export_bundle_with_soundtrack_bytes(
            &mut Explanation::new(false),
            opts.clone(),
            &settings,
            fs.as_ref(),
        )
        .unwrap();
        assert_eq!(silent.bundle.bytes, plain.bundle.bytes);
        assert!(silent.soundtrack.is_none());
        let native = export_bundle_with_soundtrack_bytes(
            &mut Explanation::new(true),
            opts,
            &settings,
            fs.as_ref(),
        )
        .unwrap();
        let receipt = native.soundtrack.unwrap();
        assert_eq!(receipt.mix.cues_mixed, 2);
        assert_eq!((receipt.mix.sample_rate, receipt.mix.channels), (8, 2));
        assert!(receipt.audio_decodes.is_empty());
        assert_eq!(receipt.inputs.len(), 2);
        for input in receipt.inputs {
            assert_eq!(
                input.bytes.as_ref(),
                fs.read(input.source.file_path().unwrap()).unwrap()
            );
            assert_eq!(input.report.format, AudioInputFormat::Wav);
            assert!(input.report.invocation.is_none());
        }
    }
    assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 0);
}

#[test]
fn compressed_admission_refusals_preserve_assets_and_publication_without_ambient_discovery() {
    use std::sync::atomic::Ordering;
    let fs = assets();
    fs.insert("/background.wav", b"fLaC source snapshot".to_vec());
    fs.insert("/existing.fmtl", b"existing artifact".to_vec());
    let (mut settings, calls) = unavailable_decoder();

    // Capability absence is preserved, even though the configured executable
    // could exist on the host. Detection follows the source bytes, not suffixes.
    settings.ffmpeg = None;
    let mut standard = options();
    standard.config.determinism.mode = DeterminismMode::Standard;
    let missing = export_bundle_with_soundtrack_with_fs(
        &mut Explanation::new(true),
        "/new.fmtl",
        standard,
        &settings,
        fs.clone(),
    )
    .unwrap_err();
    assert!(matches!(
        missing,
        BundleExportError::Soundtrack(RenderError::SoundDecode {
            source: Some(ref source),
            error: AudioDecodeError::TranscoderRequired {
                format: AudioInputFormat::Flac,
            },
        }) if matches!(source.as_ref(), fmn_scene::SoundSourceKey::File(path) if path == Path::new("/background.wav")),
    ));
    let (settings, certified_calls) = unavailable_decoder();
    let mut certified = options();
    certified.config.determinism.mode = DeterminismMode::Certified;
    let refused = export_bundle_with_soundtrack_with_fs(
        &mut Explanation::new(true),
        "/new.fmtl",
        certified,
        &settings,
        fs.clone(),
    )
    .unwrap_err();
    assert!(matches!(
        refused,
        BundleExportError::Soundtrack(RenderError::SoundDecode {
            error: AudioDecodeError::Capability(ref detail),
            ..
        }) if detail.contains("flac") && detail.contains("certified soundtrack preparation"),
    ));
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    assert_eq!(certified_calls.load(Ordering::SeqCst), 0);
    assert!(fs.read(Path::new("/new.fmtl")).is_err());
    assert_eq!(
        fs.read(Path::new("/existing.fmtl")).unwrap(),
        b"existing artifact",
    );
}

#[test]
fn invalid_export_policy_and_unrecognized_sources_precede_decoder_resolution() {
    use std::sync::atomic::Ordering;
    let fs = assets();
    for invalid in 0..6 {
        let (mut settings, calls) = unavailable_decoder();
        let mut opts = options();
        match invalid {
            0 => settings.ffmpeg_limits.timeout = std::time::Duration::ZERO,
            1 => settings.ffmpeg_limits.max_log_bytes = 0,
            2 => settings.ffmpeg_limits.max_artifact_bytes = 0,
            3 => opts.config.file_writer.ffmpeg_bin.clear(),
            4 => opts.config.file_writer.ffmpeg_bin.push('\0'),
            _ => opts.max_output_bytes = 0,
        }
        let mut program = Explanation::new(true);
        assert!(
            export_bundle_with_soundtrack_bytes(&mut program, opts, &settings, fs.as_ref())
                .is_err()
        );
        assert_eq!(program.calls.get(), 0);
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }
    for source in [
        b"#EXTM3U\nhttps://example.invalid/narration.mp3".to_vec(),
        encode_wav(1, 8, SampleFormat::F32, &[0.25])[..20].to_vec(),
    ] {
        fs.insert("/background.wav", source);
        let (settings, calls) = unavailable_decoder();
        assert!(
            export_bundle_with_soundtrack_with_fs(
                &mut Explanation::new(true),
                "/new.fmtl",
                options(),
                &settings,
                fs.clone(),
            )
            .is_err()
        );
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        assert!(fs.read(Path::new("/new.fmtl")).is_err());
    }
}

// The governed boundary admits private tool directories on these native hosts.
// The process mechanism is scripted: neither this image nor ffmpeg is executed.
#[cfg(all(
    any(target_os = "linux", target_os = "macos"),
    any(target_arch = "x86_64", target_arch = "aarch64")
))]
mod compressed {
    use super::*;

    const VOICE: &[u8] = b"fLaC narration fixture";
    const CHIME: &[u8] = b"OggS chime fixture";

    struct Narration {
        calls: Rc<Cell<usize>>,
    }

    impl Narration {
        fn new() -> Self {
            Self {
                calls: Rc::new(Cell::new(0)),
            }
        }
    }

    impl SceneConstruct for Narration {
        fn construct(&mut self, stage: &mut Stage<'_>) -> fmn::Result<()> {
            self.calls.set(self.calls.get() + 1);
            stage.add(Circle::new().radius(0.5))?;
            stage
                .scene_mut()
                .add_sound("/voice.asset", -0.125, Some(-6.0), None)?;
            stage.wait(0.25)?;
            stage
                .scene_mut()
                .add_sound("/chime.asset", 0.0, None, Some(-3.0))?;
            stage
                .scene_mut()
                .add_sound("/voice.asset", -0.125, Some(-3.0), None)?;
            stage.wait(0.25)?;
            Ok(())
        }
    }

    fn source_assets(compressed: bool) -> Arc<VirtualFs> {
        let fs = Arc::new(VirtualFs::new());
        let pcm = lead();
        let wav = encode_wav(
            pcm.channels,
            pcm.sample_rate,
            SampleFormat::F32,
            &pcm.samples,
        );
        fs.insert(
            "/voice.asset",
            if compressed {
                VOICE.to_vec()
            } else {
                wav.clone()
            },
        );
        fs.insert(
            "/chime.asset",
            if compressed { CHIME.to_vec() } else { wav },
        );
        fs
    }

    use crate::compressed_audio::Fixture;

    impl Fixture {
        fn options(&self) -> BundleExportOptions {
            let mut opts = options();
            opts.config.determinism.mode = DeterminismMode::Standard;
            opts.config.file_writer.ffmpeg_bin = self.tool.to_str().unwrap().to_owned();
            opts
        }

        fn audio(&self) -> BundleSoundtrackOptions {
            let mut settings = audio(4, 8);
            settings.ffmpeg = Some(self.capability.clone());
            settings.ffmpeg_limits.timeout = std::time::Duration::from_secs(11);
            settings.ffmpeg_limits.max_log_bytes = 1024;
            settings.ffmpeg_limits.max_artifact_bytes = 65_536;
            settings
        }
    }

    fn decoded_wav() -> Vec<u8> {
        let pcm = lead();
        encode_wav(
            pcm.channels,
            pcm.sample_rate,
            SampleFormat::F32,
            &pcm.samples,
        )
    }

    #[test]
    fn compressed_exports_keep_pcm_source_snapshots_and_decoder_receipts() {
        for compact in [false, true] {
            let fs = source_assets(true);
            let fixture = Fixture::new(Some(Arc::clone(&fs)), decoded_wav(), false);
            let mut opts = fixture.options();
            if compact {
                opts.storage = BundleStorage::Compact(CompactRecordingLimits::default());
            }
            let reference = export_bundle_with_soundtrack_bytes(
                &mut Narration::new(),
                opts.clone(),
                &audio(1, 8),
                source_assets(false).as_ref(),
            )
            .unwrap();
            let mut program = Narration::new();
            let result = export_bundle_with_soundtrack_bytes(
                &mut program,
                opts,
                &fixture.audio(),
                fs.as_ref(),
            )
            .unwrap();
            assert_eq!(program.calls.get(), 1);
            assert_eq!(result.bundle.bytes, reference.bundle.bytes);
            let receipt = result.soundtrack.unwrap();
            let native = reference.soundtrack.unwrap();
            assert_eq!(receipt.mix, native.mix);
            assert_eq!(receipt.inputs.len(), 3);
            assert_eq!(receipt.mix.cues_mixed, 3);
            assert_eq!(receipt.audio_decodes.len(), 2);
            assert_eq!(receipt.inputs[0].bytes.as_ref(), VOICE);
            assert_eq!(receipt.inputs[1].bytes.as_ref(), CHIME);
            assert_eq!(receipt.inputs[0].report.format, AudioInputFormat::Flac);
            assert_eq!(receipt.inputs[1].report.format, AudioInputFormat::Ogg);
            assert_eq!(
                receipt.inputs[0].report.source_digest.to_hex(),
                "3daec786d3d1a4e3081a313f6aa5da2a295621dff7cab75d45ea89ee72465827",
            );
            assert_eq!(
                receipt.inputs[1].report.source_digest.to_hex(),
                "26e206f3948c318f09916fbcda61cc6e748f1fab7bed6ce7880d3e423459650a",
            );
            assert_eq!(
                receipt.inputs[0].report.pcm_digest,
                native.inputs[0].report.pcm_digest,
            );
            assert!(Arc::ptr_eq(
                &receipt.inputs[0].bytes,
                &receipt.inputs[2].bytes
            ));
            assert!(Arc::ptr_eq(
                &receipt.inputs[0].report,
                &receipt.inputs[2].report,
            ));
            assert!(!Arc::ptr_eq(
                &receipt.inputs[0].report,
                &receipt.inputs[1].report,
            ));
            let input_invocation = receipt.inputs[0].report.invocation.as_ref().unwrap();
            assert_eq!(input_invocation, &receipt.audio_decodes[0]);
            assert_eq!(
                input_invocation.stderr.as_slice(),
                b"fixture decoded original source bytes",
            );
            assert_eq!(
                fixture.locates.lock().unwrap().as_slice(),
                std::slice::from_ref(&fixture.tool),
            );
            assert_eq!(
                fixture.sources.lock().unwrap().as_slice(),
                [VOICE.to_vec(), CHIME.to_vec()],
            );
            let runs = fixture.runs.lock().unwrap();
            assert_eq!(
                runs.len(),
                3,
                "one identity probe and two distinct source decodes",
            );
            for run in &runs[1..] {
                assert_eq!(run.timeout, std::time::Duration::from_secs(11));
                assert_eq!(run.max_output_bytes, 1024);
            }
            for invocation in &receipt.audio_decodes {
                assert_eq!(invocation.provenance.tool_path, fixture.tool);
                assert_eq!(
                    invocation.provenance.tool_version,
                    "ffmpeg version bundle-audio-fixture",
                );
                assert_eq!(invocation.provenance.tool_sha256_hex.len(), 64);
                assert_eq!(
                    invocation.provenance.process_mechanism,
                    "scripted.process_runner",
                );
                assert!(!invocation.artifact.exists());
            }
            assert_eq!(
                fs.read(Path::new("/voice.asset")).unwrap(),
                b"changed during first decode",
            );
            let mut portable = TimelineBundle::from_bytes(&result.bundle.bytes).unwrap();
            let pcm = portable.take_soundtrack().unwrap();
            assert_eq!(pcm.sample_frames(), receipt.mix.sample_frames);
            assert_eq!(pcm.samples().len() as u64, receipt.mix.sample_frames * 2);
        }
    }

    #[test]
    fn compressed_camera_publication_keeps_receipts_and_no_clobber() {
        let fs = source_assets(true);
        let fixture = Fixture::new(None, decoded_wav(), false);
        let camera = fmn_render::CameraConfig {
            fps: 8,
            resolution: (64, 36),
            ..Default::default()
        };
        let factory = |scene: &mut Scene, camera: &fmn_render::CameraConfig| {
            Ok((Narration::new(), CameraRig::new(scene, camera)?))
        };
        let receipt = export_camera_bundle_with_soundtrack_with_fs(
            factory,
            "/camera.fmtl",
            camera.clone(),
            fixture.options(),
            &fixture.audio(),
            fs.clone(),
        )
        .unwrap();
        let bytes = fs.read(Path::new("/camera.fmtl")).unwrap();
        let bundle = TimelineBundle::from_bytes(&bytes).unwrap();
        assert!(bundle.has_camera_track());
        assert_eq!(receipt.bytes, bytes.len());
        let sound = receipt.soundtrack.unwrap();
        assert_eq!(sound.audio_decodes.len(), 2);
        assert_eq!(sound.inputs[0].bytes.as_ref(), VOICE);
        assert!(
            export_camera_bundle_with_soundtrack_with_fs(
                factory,
                "/camera.fmtl",
                camera,
                fixture.options(),
                &fixture.audio(),
                fs.clone(),
            )
            .is_err()
        );
        assert_eq!(fs.read(Path::new("/camera.fmtl")).unwrap(), bytes);
    }

    #[test]
    fn compressed_decode_and_expansion_failures_never_publish_a_partial_bundle() {
        for failure in 0..5 {
            let fs = source_assets(true);
            fs.insert("/existing.fmtl", b"keep existing artifact".to_vec());
            let output = if failure == 1 {
                encode_wav(1, 8, SampleFormat::F32, &[f32::NAN])
            } else {
                decoded_wav()
            };
            let fixture = Fixture::new(None, output, failure == 0);
            let mut settings = fixture.audio();
            match failure {
                2 => settings.decode.max_samples = 2,
                3 => settings.decode.max_input_bytes = 4,
                4 => settings.ffmpeg_limits.max_artifact_bytes = 8,
                _ => {}
            }
            let error = export_bundle_with_soundtrack_with_fs(
                &mut Narration::new(),
                "/new.fmtl",
                fixture.options(),
                &settings,
                fs.clone(),
            )
            .unwrap_err();
            assert!(
                matches!(
                    &error,
                    BundleExportError::Soundtrack(RenderError::SoundDecode { .. }),
                ),
                "failure {failure}: {error}",
            );
            assert!(fs.read(Path::new("/new.fmtl")).is_err());
            assert_eq!(
                fs.read(Path::new("/existing.fmtl")).unwrap(),
                b"keep existing artifact",
            );
            assert_eq!(
                fixture.locates.lock().unwrap().len(),
                usize::from(failure != 3),
            );
        }
    }
}
