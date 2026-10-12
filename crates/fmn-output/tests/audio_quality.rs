//! Audio controls affect only the AAC mux, never the video encode or native PCM.
use fmn_output::negotiate::{encode_argv, mux_argv, mux_argv_with_quality};
use fmn_output::{AudioQuality, ColorDescription, Container, EncoderChoice, VideoJob, WireFormat};
use std::path::Path;

#[test]
fn aac_mux_preserves_defaults_and_stream_copy_with_explicit_targets() {
    let video = Path::new("/private job/video.mp4");
    let audio = Path::new("/private job/soundtrack.wav");
    let output = Path::new("/private job/muxed.mp4");
    let defaults = mux_argv(video, audio, output);
    assert_eq!(
        mux_argv_with_quality(video, audio, output, AudioQuality::default()).unwrap(),
        defaults
    );
    assert!(!defaults.iter().any(|arg| arg == "-b:a"));
    for bitrate in [1, 96_000, 192_000, 320_000, u32::MAX] {
        let quality = AudioQuality {
            bitrate: Some(bitrate),
        };
        let argv = mux_argv_with_quality(video, audio, output, quality).unwrap();
        assert_eq!(argv.iter().filter(|arg| *arg == "-b:a").count(), 1);
        let at = argv.iter().position(|arg| arg == "-b:a").unwrap();
        assert_eq!(argv[at + 1], bitrate.to_string());
        assert!(argv.windows(2).any(|p| p == ["-c:v", "copy"]));
        assert!(argv.windows(2).any(|p| p == ["-c:a", "aac"]));
        assert!(argv.windows(2).any(|p| p == ["-map", "0:v:0"]));
        assert!(argv.windows(2).any(|p| p == ["-map", "1:a:0"]));
        let mut without_override = argv;
        without_override.drain(at..at + 2);
        assert_eq!(without_override, defaults);
    }
    assert!(
        mux_argv_with_quality(video, audio, output, AudioQuality { bitrate: Some(0) }).is_err()
    );
}

#[test]
fn audio_controls_do_not_enter_the_video_encoder_arguments() {
    for container in [Container::Mp4, Container::Mov, Container::MovTransparent] {
        let job = VideoJob {
            width: 64,
            height: 36,
            fps: (30_000, 1_001),
            wire: WireFormat::Rgba8,
            color: ColorDescription::srgb_full(),
            container,
            encoder: EncoderChoice::Auto,
            crf: None,
        };
        let argv = encode_argv(&job, Path::new("/video.mov")).unwrap();
        assert!(!argv.iter().any(|arg| arg == "-b:a" || arg == "-c:a"));
    }
}

#[cfg(all(unix, feature = "ffmpeg-test-fixture"))]
#[test]
fn aac_policy_reaches_actual_streaming_mux_and_deferred_wav() {
    use fmn_output::{Boundary, EncoderCapabilities, FfmpegTool, JobLimits};
    use fmn_platform::process::{
        FfmpegLocator, ProcessCancellation, ProcessStdinLimits, StdFfmpegLocator, StdProcessRunner,
    };
    use std::os::unix::fs::PermissionsExt;
    use std::sync::Arc;
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let root = std::env::temp_dir()
        .canonicalize()
        .unwrap()
        .join(format!("fmn-aac-{}-{nonce}", std::process::id()));
    std::fs::create_dir(&root).unwrap();
    std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();
    let executable = StdFfmpegLocator::default()
        .locate_ffmpeg(Path::new(env!("CARGO_BIN_EXE_fmn-ffmpeg-test-fixture")))
        .unwrap();
    let runner = Arc::new(StdProcessRunner);
    let tool = FfmpegTool::resolve(executable, runner.as_ref(), &root).unwrap();
    let caps = EncoderCapabilities::probe(&tool, runner.as_ref()).unwrap();
    let job = VideoJob {
        width: 2,
        height: 2,
        fps: (30, 1),
        wire: WireFormat::Rgba8,
        color: ColorDescription::srgb_full(),
        container: Container::Mp4,
        encoder: EncoderChoice::Auto,
        crf: None,
    };
    let wav = fmn_codec::encode_wav(1, 48_000, fmn_codec::SampleFormat::S16, &[0.25; 480]);
    let audio = root.join("soundtrack.wav");
    std::fs::write(&audio, &wav).unwrap();
    for (index, bitrate) in [None, Some(96_000), Some(192_000)].into_iter().enumerate() {
        for deferred in [false, true] {
            let boundary = Boundary::new(
                tool.clone(),
                runner.clone(),
                JobLimits {
                    keep_workdir: true,
                    ..JobLimits::default()
                },
                root.clone(),
            )
            .unwrap()
            .with_audio_quality(AudioQuality { bitrate })
            .unwrap();
            let destination = root.join(format!("{index}-{deferred}.mp4"));
            let mut stream = boundary
                .start_encode(
                    &job,
                    &caps,
                    &destination,
                    if deferred { None } else { Some(&audio) },
                    ProcessCancellation::new(),
                    ProcessStdinLimits::new(16, 16),
                )
                .unwrap();
            stream.write_stdin(&[0; 16]).unwrap();
            let prepared = if deferred {
                stream.prepare_with_wav(&wav)
            } else {
                stream.prepare()
            }
            .unwrap();
            assert!(!destination.exists());
            let report = prepared.commit().unwrap();
            assert_eq!(report.invocations.len(), 2);
            let video = &report.invocations[0].provenance.argv;
            let mux = &report.invocations[1].provenance.argv;
            assert!(!video.iter().any(|arg| arg == "-b:a"));
            assert!(mux.windows(2).any(|p| p == ["-c:v", "copy"]));
            let actual = mux
                .iter()
                .position(|arg| arg == "-b:a")
                .map(|at| mux[at + 1].parse::<u32>().unwrap());
            assert_eq!(actual, bitrate);
            let logged = std::fs::read_to_string(
                report.invocations[1]
                    .artifact
                    .parent()
                    .unwrap()
                    .join("argv.log"),
            )
            .unwrap();
            assert_eq!(logged.lines().last().unwrap(), mux.join(" "));
            assert!(destination.is_file());
        }
    }
    // The real boundary, not just the pure builder, rejects invalid policy.
    assert!(
        Boundary::new(tool, runner, JobLimits::default(), root)
            .unwrap()
            .with_audio_quality(AudioQuality { bitrate: Some(0) })
            .is_err()
    );
}
