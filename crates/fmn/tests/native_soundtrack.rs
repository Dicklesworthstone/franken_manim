//! Exercise scene-authored native WAV output without a raster or video pipeline.

use std::cell::Cell;
use std::num::NonZeroUsize;
use std::path::Path;
use std::rc::Rc;
use std::sync::Arc;

use fmn::prelude::*;
use fmn::{SoundtrackRenderOptions, render_soundtrack_with_fs};
use fmn_codec::{SampleFormat, WavLimits, decode_wav, encode_wav};
use fmn_config::config::{DeterminismMode, Engine};
use fmn_output::{AudioDecodeError, SoundError};
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
    for invalid in 0..8 {
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
            _ => request.output = "/".into(),
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
                    path,
                    error: AudioDecodeError::Native(_),
                },
            )
            | (
                2,
                RenderError::SoundDecode {
                    path,
                    error: AudioDecodeError::TranscoderRequired { .. },
                },
            ) => assert_eq!(path, Path::new(source)),
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
