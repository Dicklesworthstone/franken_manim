//! Native scenes compile into portable audible recordings and replay without assets.
use fmn::exporting::soundtrack::*;
use fmn::exporting::{BundleExportOptions, export_bundle_bytes, export_camera_bundle_bytes};
use fmn::prelude::*;
use fmn::rendering::{SoundtrackPreparationLimits, render_bundle_with_fs};
use fmn_codec::{SampleFormat, WavAudio, encode_wav};
use fmn_output::{AudioDecodeLimits, AudioDecoder, MixerConfig, SoundCue, SoundMixer};
use fmn_platform::fs::{FileSystem, StdFs, VirtualFs};
use fmn_platform::process::{FfmpegLocator, StdFfmpegLocator, StdProcessRunner};
use fmn_scene::{CameraRig, TimelineBundle};
use std::{cell::Cell, path::Path, rc::Rc, sync::Arc};

fn background() -> WavAudio {
    WavAudio {
        channels: 1,
        sample_rate: 8,
        format: SampleFormat::F32,
        samples: vec![0.125, 0.25, 0.375, 0.5, 0.25, 0.0, -0.125, -0.25],
    }
}
fn lead() -> WavAudio {
    WavAudio {
        channels: 2,
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
    std::fs::create_dir_all(&root).unwrap();
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
    for (hold, pictures, expected) in [
        (true, 4, vec![0.25, 0.25, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0]),
        (false, 1, vec![0.25, 0.25]),
    ] {
        let export =
            export_bundle_with_soundtrack_bytes(&mut Short { hold }, options(), &audio(1, 8), &fs)
                .unwrap();
        assert_eq!(export.bundle.frame_count, pictures);
        assert_eq!(export.scene.time.frames(), if hold { 4 } else { 0 });
        let bundle = TimelineBundle::from_bytes(&export.bundle.bytes).unwrap();
        assert_eq!(bundle.soundtrack().unwrap().samples(), expected);
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
            sound_file: "/repeat.wav".into(),
            time: clock.now(),
            time_offset: 0.0,
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
