//! Exercise scene-authored native WAV output without a raster or video pipeline.

#[allow(dead_code)]
#[path = "support/surround_audio.rs"]
mod surround_audio;

#[cfg(all(
    any(target_os = "linux", target_os = "macos"),
    any(target_arch = "x86_64", target_arch = "aarch64")
))]
#[path = "support/compressed_audio.rs"]
mod compressed_audio;

use std::cell::Cell;
use std::num::NonZeroUsize;
use std::path::Path;
use std::rc::Rc;
use std::sync::Arc;

use fmn::prelude::*;
use fmn::{SoundtrackRenderOptions, render_soundtrack_with_fs};
use fmn_codec::{SampleFormat, WavLimits, decode_wav, encode_wav};
use fmn_config::config::{DeterminismMode, Engine};
use fmn_output::{AudioDecodeError, DitherPolicy, SoundError};
use fmn_platform::fs::{FileSystem, VirtualFs};

fn options(path: &str, threads: usize) -> SoundtrackRenderOptions {
    let mut options = SoundtrackRenderOptions::new(path).unwrap();
    options.config.camera.fps = 8;
    // These would refuse a pixel render. Audio uses neither field and must
    // not create a fictitious camera or accelerator execution plan.
    options.config.camera.resolution = (0, 0);
    options.config.render.engine = Engine::Metal;
    options.config.determinism.mode = DeterminismMode::Certified;
    options.mixer.sample_rate = 32;
    options.mixer.channels = 1;
    options.mixer.max_output_frames = 256;
    options.threads = NonZeroUsize::new(threads).unwrap();
    options.typeset_cache = false;
    options
}

fn sources() -> Arc<VirtualFs> {
    let fs = Arc::new(VirtualFs::new());
    fs.insert(
        "/background.wav",
        encode_wav(1, 32, SampleFormat::F32, &[0.25; 24]),
    );
    fs.insert("/lead.wav", encode_wav(1, 32, SampleFormat::F32, &[1.0; 8]));
    fs
}

#[cfg(all(
    any(target_os = "linux", target_os = "macos"),
    any(target_arch = "x86_64", target_arch = "aarch64")
))]
mod compressed_sources {
    use super::*;
    use crate::compressed_audio::Fixture;
    use fmn_output::{AudioDecodeLimits, AudioDecoder, AudioInputFormat};

    const VOICE: &[u8] = b"fLaC narration fixture";

    struct Narration;

    impl SceneConstruct for Narration {
        fn construct(&mut self, stage: &mut Stage<'_>) -> fmn::Result<()> {
            stage
                .scene_mut()
                .add_sound("/voice.asset", 0.0, None, None)?;
            stage.wait(0.25)?;
            stage
                .scene_mut()
                .add_sound("/voice.asset", -0.125, Some(-6.0), Some(-3.0))?;
            stage.wait(0.25)?;
            Ok(())
        }
    }

    fn decoded_wav() -> Vec<u8> {
        encode_wav(
            1,
            32,
            SampleFormat::F32,
            &[0.25, -0.5, 0.75, 0.0, 0.125, 0.25, -0.25, 0.5],
        )
    }

    fn request(fixture: &Fixture, output: &str, workers: usize) -> SoundtrackRenderOptions {
        let mut settings = options(output, workers);
        settings.config.determinism.mode = DeterminismMode::Standard;
        settings.config.file_writer.ffmpeg_bin = fixture.tool.to_str().unwrap().to_owned();
        settings.ffmpeg = Some(fixture.capability.clone());
        settings.ffmpeg_limits.timeout = std::time::Duration::from_secs(11);
        settings.ffmpeg_limits.max_log_bytes = 73;
        settings.ffmpeg_limits.max_artifact_bytes = 65_536;
        settings.decode.max_input_bytes = VOICE.len() as u64;
        settings.dither = DitherPolicy::Tpdf { seed: 53 };
        settings
    }

    #[test]
    fn compressed_wav_matches_native_mix_and_retains_exact_decode_receipts_at_all_workers() {
        let wav = decoded_wav();
        let expected_pcm = AudioDecoder::new(AudioDecodeLimits::default(), None)
            .unwrap()
            .decode(&wav)
            .unwrap()
            .report
            .pcm_digest;
        let native = Arc::new(VirtualFs::new());
        native.insert("/voice.asset", wav.clone());
        let mut reference = options("/reference.wav", 1);
        reference.dither = DitherPolicy::Tpdf { seed: 53 };
        render_soundtrack_with_fs(&mut Narration, reference, native.clone()).unwrap();
        let expected = native.read(Path::new("/reference.wav")).unwrap();
        for workers in [1, 4, 16] {
            let fs = Arc::new(VirtualFs::new());
            fs.insert("/voice.asset", VOICE.to_vec());
            let fixture = Fixture::new(Some(fs.clone()), wav.clone(), false);
            let report = render_soundtrack_with_fs(
                &mut Narration,
                request(&fixture, "/voice.wav", workers),
                fs.clone(),
            )
            .unwrap();
            assert_eq!(fs.read(Path::new("/voice.wav")).unwrap(), expected);
            assert_eq!(report.artifact.digest, fmn_hash::sha256(&expected));
            assert_eq!(report.inputs.len(), 2);
            assert!(Arc::ptr_eq(
                &report.inputs[0].bytes,
                &report.inputs[1].bytes
            ));
            assert_eq!(report.inputs[0].bytes.as_ref(), VOICE);
            assert_eq!(
                report.inputs[0].report.source_digest,
                fmn_hash::sha256(VOICE)
            );
            assert_eq!(report.inputs[0].report.pcm_digest, expected_pcm);
            assert_eq!(report.inputs[0].report.format, AudioInputFormat::Flac);
            assert_eq!(report.audio_decodes.len(), 1);
            let invocation = &report.audio_decodes[0];
            assert_eq!(invocation.provenance.tool_path, fixture.tool);
            assert_eq!(
                invocation.provenance.tool_sha256_hex,
                fmn_hash::sha256(&std::fs::read(&fixture.tool).unwrap()).to_hex()
            );
            assert!(
                invocation
                    .provenance
                    .tool_version
                    .contains("bundle-audio-fixture")
            );
            assert!(
                invocation
                    .provenance
                    .argv
                    .windows(2)
                    .any(|p| p == ["-f", "flac"])
            );
            assert!(
                !invocation
                    .provenance
                    .argv
                    .iter()
                    .any(|arg| arg == "-ar" || arg == "-ac")
            );
            assert_eq!(*fixture.locates.lock().unwrap(), vec![fixture.tool.clone()]);
            assert_eq!(*fixture.sources.lock().unwrap(), vec![VOICE.to_vec()]);
            let runs = fixture.runs.lock().unwrap();
            assert_eq!(
                runs.len(),
                2,
                "one version probe and one distinct source decode"
            );
            assert_eq!(runs[1].timeout, std::time::Duration::from_secs(11));
            assert_eq!(runs[1].max_output_bytes, 73);
            assert!(
                report
                    .ffmpeg_limits
                    .to_json()
                    .contains("\"max_input_bytes\":22")
            );
        }
    }

    #[test]
    fn native_malformed_unknown_and_certified_sources_never_resolve_the_optional_tool() {
        let fixture = Fixture::new(None, decoded_wav(), false);
        for (index, source) in [
            decoded_wav(),
            decoded_wav()[..20].to_vec(),
            b"#EXTM3U\nhttps://example.invalid/voice.mp3".to_vec(),
            VOICE.to_vec(),
        ]
        .into_iter()
        .enumerate()
        {
            let fs = Arc::new(VirtualFs::new());
            fs.insert("/voice.asset", source);
            let mut settings = request(&fixture, "/voice.wav", 1);
            settings.decode = AudioDecodeLimits::default();
            settings.config.file_writer.ffmpeg_bin = "/unavailable/ffmpeg".into();
            if index == 3 {
                settings.config.determinism.mode = DeterminismMode::Certified;
            }
            let result = render_soundtrack_with_fs(&mut Narration, settings, fs.clone());
            match index {
                0 => assert!(result.unwrap().audio_decodes.is_empty()),
                1 => assert!(matches!(
                    result,
                    Err(RenderError::SoundDecode {
                        error: AudioDecodeError::Native(_),
                        ..
                    })
                )),
                2 => assert!(matches!(
                    result,
                    Err(RenderError::SoundDecode {
                        error: AudioDecodeError::UnsupportedInput,
                        ..
                    })
                )),
                _ => assert!(matches!(result, Err(RenderError::SoundDecode {
                    error: AudioDecodeError::Capability(detail), ..
                }) if detail.contains("certified soundtrack preparation"))),
            }
            assert_eq!(fs.exists(Path::new("/voice.wav")), index == 0);
        }
        assert!(fixture.locates.lock().unwrap().is_empty());
        assert!(fixture.runs.lock().unwrap().is_empty());
    }

    #[test]
    fn compressed_source_decode_expansion_and_publication_failures_leave_no_wav() {
        for case in 0..6 {
            let fs = Arc::new(VirtualFs::new());
            fs.insert("/voice.asset", VOICE.to_vec());
            fs.insert("/preserved.wav", b"existing bytes".to_vec());
            let fixture = Fixture::new(None, decoded_wav(), case == 4);
            let mut settings = request(&fixture, "/refused.wav", 1);
            match case {
                0 => settings.decode.max_input_bytes = VOICE.len() as u64 - 1,
                1 => settings.decode.max_samples = 1,
                2 => settings.preparation.max_prepared_samples = 1,
                3 => settings.ffmpeg_limits.max_artifact_bytes = 8,
                4 => {}
                _ => settings.ffmpeg = None,
            }
            let error =
                render_soundtrack_with_fs(&mut Narration, settings, fs.clone()).unwrap_err();
            if case == 5 {
                assert!(matches!(
                    error,
                    RenderError::SoundDecode {
                        error: AudioDecodeError::TranscoderRequired {
                            format: AudioInputFormat::Flac
                        },
                        ..
                    }
                ));
            }
            if case == 0 || case == 5 {
                assert!(fixture.locates.lock().unwrap().is_empty());
            }
            assert!(!fs.exists(Path::new("/refused.wav")));
            assert_eq!(
                fs.read(Path::new("/preserved.wav")).unwrap(),
                b"existing bytes"
            );
        }
    }
}

struct Mixed(Rc<Cell<u32>>);

impl SceneConstruct for Mixed {
    fn construct(&mut self, stage: &mut Stage<'_>) -> fmn::Result<()> {
        let circle = stage.add(Circle::new())?;
        let calls = Rc::clone(&self.0);
        stage.arena_mut().add_dt_updater(
            circle,
            move |arena, mob, dt| {
                if dt > 0.0 {
                    calls.set(calls.get() + 1);
                }
                arena.shift(mob, [dt, 0.0, 0.0]);
            },
            false,
        )?;
        stage
            .scene_mut()
            .add_sound("/background.wav", 0.0, None, None)?;
        stage.wait(0.25)?;
        // Half amplitude, twice at the same rational call site. The second
        // overlay ducks the first too, proving authored insertion order.
        for _ in 0..2 {
            stage.scene_mut().add_sound(
                "/lead.wav",
                -0.125,
                Some(-6.020_599_913_279_624),
                Some(-6.020_599_913_279_624),
            )?;
        }
        stage.wait(0.25)?;
        Ok(())
    }
}

#[test]
fn native_wav_keeps_updates_offsets_ducking_and_tails_at_every_worker_count() {
    let fs = sources();
    let mut expected = vec![0.25; 24];
    expected[4..12].fill(0.8125);
    let expected = encode_wav(1, 32, SampleFormat::S16, &expected);
    let mut digest = None;
    for threads in [1, 4, 16] {
        let path = format!("/mix-{threads}.wav");
        let calls = Rc::new(Cell::new(0));
        let mut request = options(&path, threads);
        request.max_scene_frames = 4;
        request.max_output_bytes = expected.len() as u64;
        let report =
            render_soundtrack_with_fs(&mut Mixed(Rc::clone(&calls)), request, fs.clone()).unwrap();
        assert_eq!(calls.get(), 4);
        assert_eq!(
            (report.scene.time.frames(), report.scene.play_count),
            (4, 2)
        );
        assert_eq!((report.sample_rate, report.channels), (32, 1));
        assert_eq!(
            report.artifact.sample_frames, 24,
            "the cue tail outlasts the scene"
        );
        assert_eq!(
            (report.artifact.cues_mixed, report.artifact.clipped_samples),
            (3, 0)
        );
        assert!(report.workers_used > 0 && report.workers_used <= threads);
        assert_eq!(report.inputs.len(), 3);
        assert!(Arc::ptr_eq(
            &report.inputs[1].bytes,
            &report.inputs[2].bytes
        ));
        assert!(
            report
                .inputs
                .iter()
                .all(|input| input.report.invocation.is_none())
        );
        assert_eq!(fs.read(Path::new(&path)).unwrap(), expected);
        assert_eq!(
            *digest.get_or_insert(report.artifact.digest),
            report.artifact.digest
        );
    }
}

struct Skipped;

impl SceneConstruct for Skipped {
    fn construct(&mut self, stage: &mut Stage<'_>) -> fmn::Result<()> {
        stage.scene_mut().set_skipping(true);
        stage.wait(1.0)?;
        // The ordinary Scene ignores cues during skipped playback, even when
        // the ignored path and offset could not be decoded or scheduled.
        stage
            .scene_mut()
            .add_sound("/missing.wav", f64::NAN, None, None)?;
        stage.scene_mut().set_skipping(false);
        stage.wait(0.25)?;
        stage.scene_mut().add_sound("/tick.wav", 0.0, None, None)?;
        stage.wait(0.25)?;
        Ok(())
    }
}

#[test]
fn native_wav_removes_skipped_time_and_rebases_cues_once() {
    let fs = Arc::new(VirtualFs::new());
    fs.insert("/tick.wav", encode_wav(1, 32, SampleFormat::F32, &[0.5; 4]));
    let mut request = options("/skipped.wav", 4);
    request.max_scene_frames = 12;
    let report = render_soundtrack_with_fs(&mut Skipped, request, fs.clone()).unwrap();
    assert_eq!(report.scene.time.frames(), 12);
    assert_eq!(report.artifact.sample_frames, 16);
    assert_eq!(report.artifact.cues_mixed, 1);
    let mut expected = vec![0.0; 16];
    expected[8..12].fill(0.5);
    assert_eq!(
        fs.read(Path::new("/skipped.wav")).unwrap(),
        encode_wav(1, 32, SampleFormat::S16, &expected)
    );
    let mut limited = options("/skipped-limited.wav", 1);
    limited.max_scene_frames = 4;
    assert!(matches!(
        render_soundtrack_with_fs(&mut Skipped, limited, fs.clone()),
        Err(RenderError::InvalidOptions(
            "scene exceeded max_scene_frames"
        ))
    ));
    assert!(!fs.exists(Path::new("/skipped-limited.wav")));
}

struct Wait(f64);

impl SceneConstruct for Wait {
    fn construct(&mut self, stage: &mut Stage<'_>) -> fmn::Result<()> {
        if self.0 > 0.0 {
            stage.wait(self.0)?;
        }
        Ok(())
    }
}

#[test]
fn cue_free_wav_covers_the_scene_and_empty_scenes_have_no_fabricated_sample() {
    let fs = Arc::new(VirtualFs::new());
    for (duration, frames) in [(1.25, 40), (0.0, 0)] {
        let path = format!("/silent-{frames}.wav");
        let mut request = options(&path, 4);
        request.mixer.channels = 2;
        let report = render_soundtrack_with_fs(&mut Wait(duration), request, fs.clone()).unwrap();
        assert_eq!(report.artifact.sample_frames, frames);
        assert_eq!(report.artifact.cues_mixed, 0);
        assert!(report.inputs.is_empty());
        let decoded =
            decode_wav(&fs.read(Path::new(&path)).unwrap(), &WavLimits::default()).unwrap();
        assert_eq!(
            (decoded.sample_rate, decoded.channels, decoded.format),
            (32, 2, SampleFormat::S16)
        );
        assert_eq!(decoded.samples, vec![0.0; frames as usize * 2]);
        if frames == 0 {
            assert_eq!(report.workers_used, 0);
            assert_eq!(report.artifact.bytes, 44);
        }
    }
}

#[test]
fn invalid_audio_options_are_refused_before_scene_construction() {
    struct Count(Rc<Cell<u32>>);
    impl SceneConstruct for Count {
        fn construct(&mut self, _: &mut Stage<'_>) -> fmn::Result<()> {
            self.0.set(self.0.get() + 1);
            Ok(())
        }
    }
    let fs = Arc::new(VirtualFs::new());
    for invalid in 0..11 {
        let calls = Rc::new(Cell::new(0));
        let mut request = options("/invalid.wav", 1);
        match invalid {
            0 => request.max_scene_frames = 0,
            1 => request.max_output_bytes = 43,
            2 => request.preparation.max_cues = 0,
            3 => request.decode.max_samples = 0,
            4 => request.mixer.channels = 3,
            5 => request.mixer.sample_rate = u32::MAX,
            6 => request.threads = NonZeroUsize::new(1025).unwrap(),
            7 => request.output = "/".into(),
            8 => request.ffmpeg_limits.timeout = std::time::Duration::ZERO,
            9 => request.ffmpeg_limits.max_log_bytes = 0,
            _ => request.ffmpeg_limits.max_artifact_bytes = 0,
        }
        assert!(
            render_soundtrack_with_fs(&mut Count(Rc::clone(&calls)), request, fs.clone()).is_err()
        );
        assert_eq!(calls.get(), 0);
        assert!(!fs.exists(Path::new("/invalid.wav")));
    }
}

#[test]
fn cue_source_preparation_and_output_limits_leave_no_artifact() {
    let fs = sources();
    for limit in 0..7 {
        let path = format!("/limited-{limit}.wav");
        let mut request = options(&path, 1);
        match limit {
            0 => request.max_scene_frames = 3,
            1 => request.preparation.max_cues = 2,
            2 => request.preparation.max_source_bytes = 1,
            3 => request.decode.max_samples = 1,
            4 => request.preparation.max_prepared_samples = 24,
            5 => request.max_output_bytes = 44 + 15 * 2,
            _ => request.mixer.max_output_frames = 23,
        }
        let error =
            render_soundtrack_with_fs(&mut Mixed(Rc::new(Cell::new(0))), request, fs.clone())
                .unwrap_err();
        match limit {
            0 => assert!(matches!(
                error,
                RenderError::InvalidOptions("scene exceeded max_scene_frames")
            )),
            1 => assert!(matches!(
                error,
                RenderError::InvalidOptions("soundtrack cue-count budget exceeded")
            )),
            2 => assert!(matches!(error, RenderError::SoundAsset { .. })),
            3 => assert!(matches!(error, RenderError::SoundDecode { .. })),
            4 => assert!(matches!(
                error,
                RenderError::InvalidOptions("soundtrack prepared-sample budget exceeded")
            )),
            _ => assert!(matches!(
                error,
                RenderError::Sound(SoundError::OutputTooLong { .. })
            )),
        }
        assert!(!fs.exists(Path::new(&path)));
    }
}

struct Cue(&'static str);

impl SceneConstruct for Cue {
    fn construct(&mut self, stage: &mut Stage<'_>) -> fmn::Result<()> {
        stage.scene_mut().add_sound(self.0, 0.0, None, None)?;
        stage.wait(0.25)?;
        Ok(())
    }
}

#[test]
fn explicit_tpdf_is_seeded_bounded_and_independent_of_mixer_workers() {
    let fs = Arc::new(VirtualFs::new());
    let quiet = vec![0.25 / 32768.0; 128];
    fs.insert("/quiet.wav", encode_wav(1, 32, SampleFormat::F32, &quiet));
    let plain =
        render_soundtrack_with_fs(&mut Cue("/quiet.wav"), options("/plain.wav", 1), fs.clone())
            .unwrap();
    assert_eq!(plain.dither, DitherPolicy::None);
    let plain_bytes = fs.read(Path::new("/plain.wav")).unwrap();
    assert_eq!(plain_bytes, encode_wav(1, 32, SampleFormat::S16, &quiet));
    assert!(plain_bytes[44..].iter().all(|byte| *byte == 0));

    let mut seeded_outputs = Vec::new();
    for seed in [0, u64::MAX] {
        let mut reference = None;
        for threads in [1, 4, 16] {
            let path = format!("/dither-{seed}-{threads}.wav");
            let mut request = options(&path, threads);
            request.dither = DitherPolicy::Tpdf { seed };
            let report =
                render_soundtrack_with_fs(&mut Cue("/quiet.wav"), request, fs.clone()).unwrap();
            assert_eq!(report.dither, DitherPolicy::Tpdf { seed });
            assert_eq!(report.artifact.sample_frames, 128);
            assert_eq!(report.artifact.clipped_samples, 0);
            let bytes = fs.read(Path::new(&path)).unwrap();
            assert_eq!(bytes.len(), plain_bytes.len());
            assert_ne!(bytes, plain_bytes, "TPDF must reach the published PCM");
            let decoded = decode_wav(&bytes, &WavLimits::default()).unwrap();
            assert!(
                decoded
                    .samples
                    .iter()
                    .all(|sample| sample.abs() <= 2.0 / 32768.0),
                "dither must stay within a few output quantization steps"
            );
            if let Some(expected) = &reference {
                assert_eq!(&bytes, expected, "worker count changed the PCM");
            } else {
                reference = Some(bytes);
            }
        }
        seeded_outputs.push(reference.unwrap());
    }
    assert_ne!(seeded_outputs[0], seeded_outputs[1], "seed was ignored");
}

#[test]
fn source_refusals_preserve_typed_causes_and_publication_never_clobbers() {
    let fs = sources();
    fs.insert("/broken.wav", b"RIFF\0\0\0\0WAVE".to_vec());
    fs.insert("/compressed.flac", b"fLaC\0\0\0\0".to_vec());
    for (source, kind) in [
        ("/missing.wav", 0),
        ("/broken.wav", 1),
        ("/compressed.flac", 2),
    ] {
        let error =
            render_soundtrack_with_fs(&mut Cue(source), options("/refused.wav", 1), fs.clone())
                .unwrap_err();
        match (kind, error) {
            (0, RenderError::SoundAsset { path, .. }) => assert_eq!(path, Path::new(source)),
            (
                1,
                RenderError::SoundDecode {
                    source: Some(identity),
                    error: AudioDecodeError::Native(_),
                },
            )
            | (
                2,
                RenderError::SoundDecode {
                    source: Some(identity),
                    error: AudioDecodeError::Capability(_),
                },
            ) => assert_eq!(
                identity.as_ref(),
                &fmn_scene::SoundSourceKey::File(source.into())
            ),
            (_, error) => panic!("source error lost its identity: {error}"),
        }
        assert!(!fs.exists(Path::new("/refused.wav")));
    }
    fs.insert("/owned.wav", b"user-owned soundtrack".to_vec());
    assert!(matches!(
        render_soundtrack_with_fs(&mut Cue("/lead.wav"), options("/owned.wav", 1), fs.clone()),
        Err(RenderError::Sink(_))
    ));
    assert_eq!(
        fs.read(Path::new("/owned.wav")).unwrap(),
        b"user-owned soundtrack"
    );
}

#[test]
fn scene_failure_is_preserved_and_the_same_destination_can_be_retried() {
    struct Failing;
    impl SceneConstruct for Failing {
        fn construct(&mut self, stage: &mut Stage<'_>) -> fmn::Result<()> {
            stage.scene_mut().add_sound("/lead.wav", 0.0, None, None)?;
            stage.wait(0.25)?;
            Err(SceneError::InvalidConfig("authored soundtrack failure").into())
        }
    }
    let fs = sources();
    assert!(matches!(
        render_soundtrack_with_fs(&mut Failing, options("/retry.wav", 1), fs.clone()),
        Err(RenderError::Scene(fmn::Error::Scene(
            SceneError::InvalidConfig("authored soundtrack failure")
        )))
    ));
    assert!(!fs.exists(Path::new("/retry.wav")));
    render_soundtrack_with_fs(&mut Cue("/lead.wav"), options("/retry.wav", 1), fs.clone()).unwrap();
    assert!(fs.exists(Path::new("/retry.wav")));
}

#[test]
fn authored_failure_precedes_a_later_cleanup_budget_refusal() {
    struct FailingBeforeCleanup;
    impl SceneConstruct for FailingBeforeCleanup {
        fn construct(&mut self, stage: &mut Stage<'_>) -> fmn::Result<()> {
            // A custom driver can use the public Scene with its own sink.
            // Our duration bound first observes this advance during teardown.
            stage
                .scene_mut()
                .wait(Some(0.5), &mut fmn_scene::NullSceneSink)?;
            Err(SceneError::InvalidConfig("authored failure before cleanup").into())
        }
    }
    let fs = Arc::new(VirtualFs::new());
    let mut request = options("/cleanup.wav", 1);
    request.max_scene_frames = 1;
    assert!(matches!(
        render_soundtrack_with_fs(&mut FailingBeforeCleanup, request, fs.clone()),
        Err(RenderError::Scene(fmn::Error::Scene(
            SceneError::InvalidConfig("authored failure before cleanup")
        )))
    ));
    assert!(!fs.exists(Path::new("/cleanup.wav")));
}

/// The README's soundtrack example, executed against the real filesystem:
/// the same scene and options, with its `assets/voice.wav` written first
/// and both paths rooted in a private directory (the README block itself is
/// compile-only, since a doctest cannot own a real asset or a fresh
/// destination).
#[test]
fn readme_soundtrack_example_renders_three_seconds_of_stereo_at_48_khz() {
    struct Narration(std::path::PathBuf);
    impl SceneConstruct for Narration {
        fn construct(&mut self, stage: &mut Stage<'_>) -> fmn::Result<()> {
            stage
                .scene_mut()
                .add_sound(self.0.clone(), 0.0, None, None)?;
            stage.wait(3.0)?;
            Ok(())
        }
    }
    let root = std::env::temp_dir().join(format!(
        "fmn-readme-soundtrack-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(root.join("assets")).unwrap();
    let voice = root.join("assets/voice.wav");
    let tone: Vec<f32> = (0..24_000)
        .map(|i| 0.25 * (f64::from(i) * 440.0 * std::f64::consts::TAU / 48_000.0).sin() as f32)
        .collect();
    std::fs::write(&voice, encode_wav(1, 48_000, SampleFormat::F32, &tone)).unwrap();
    let output = root.join("media/soundtrack.wav");
    let mut audio = SoundtrackRenderOptions::new(&output).unwrap();
    audio.mixer.sample_rate = 48_000;
    audio.mixer.channels = 2;
    let report = fmn::render_soundtrack(&mut Narration(voice), audio).unwrap();
    assert_eq!(report.artifact.sample_frames, 3 * 48_000);
    assert_eq!((report.sample_rate, report.channels), (48_000, 2));
    assert_eq!(report.artifact.cues_mixed, 1);
    assert_eq!(report.artifact.path, output);
    let decoded = decode_wav(&std::fs::read(&output).unwrap(), &WavLimits::default()).unwrap();
    assert_eq!((decoded.sample_rate, decoded.channels), (48_000, 2));
}

#[test]
fn surround_sources_reach_the_public_wav_path_with_explicit_layouts() {
    let checked = surround_audio::verify_wav().unwrap();
    assert_eq!(checked.renders, 24);
    assert_eq!(checked.refusals, 3);
    assert!(!checked.wav.is_empty());
}
