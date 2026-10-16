//! NV12 passthrough is chosen by the encoder's actual format negotiation,
//! not a guessed hardware-vendor capability table.
#![forbid(unsafe_code)]

use std::path::Path;

use fmn_output::negotiate::{VideoQuality, encode_argv};
use fmn_output::{ColorDescription, Container, EncoderChoice, VideoJob, WireFormat};

fn job(name: &str, wire: WireFormat, container: Container) -> VideoJob {
    VideoJob {
        width: 1920,
        height: 1080,
        fps: (60_000, 1_001),
        wire,
        color: ColorDescription::video_bt709(),
        container,
        encoder: EncoderChoice::Named(name.to_owned()),
        crf: None,
    }
}

fn formats(argv: &[String]) -> Vec<&str> {
    argv.windows(2)
        .filter(|pair| pair[0] == "-pix_fmt")
        .map(|pair| pair[1].as_str())
        .collect()
}

#[test]
fn native_nv12_encoders_are_not_forced_through_planar_chroma() {
    // The unrecognized name is intentional: this must work for future
    // encoder implementations too, without a hard-coded brand whitelist.
    for name in [
        "h264_nvenc",
        "hevc_nvenc",
        "av1_nvenc",
        "h264_videotoolbox",
        "hevc_videotoolbox",
        "h264_qsv",
        "hevc_qsv",
        "libx265",
        "future_nv12_encoder",
    ] {
        for container in [Container::Mp4, Container::Mov] {
            let original = job(name, WireFormat::Nv12, container);
            for choice in [
                EncoderChoice::Named(name.to_owned()),
                EncoderChoice::Configured {
                    name: name.to_owned(),
                    quality: VideoQuality::default(),
                },
            ] {
                let mut configured = original.clone();
                configured.encoder = choice;
                let argv = encode_argv(&configured, Path::new("/out/movie.mov")).unwrap();
                assert_eq!(formats(&argv), ["nv12"], "{name}: {argv:?}");
                let input = argv.iter().position(|arg| arg == "-i").unwrap();
                let format = argv.iter().position(|arg| arg == "-pix_fmt").unwrap();
                assert!(format < input);
                assert!(argv.windows(2).any(|pair| pair == ["-c:v", name]));
                assert!(
                    argv.windows(2)
                        .any(|pair| pair == ["-framerate", "60000/1001"])
                );
                assert!(argv.windows(2).any(|pair| pair == ["-color_range", "tv"]));
                assert!(!argv.iter().any(|arg| matches!(
                    arg.as_str(),
                    "-vf" | "-filter:v" | "-filter_complex" | "-hwaccel" | "-hwaccel_output_format"
                )));
                assert_eq!(argv.last().unwrap(), "/out/movie.mov");
                assert_eq!(configured.wire, WireFormat::Nv12);
                assert_eq!(configured.color, original.color);
            }
        }
    }
}

#[test]
fn software_default_and_non_nv12_profiles_remain_explicit() {
    for container in [Container::Mp4, Container::Mov] {
        for choice in [EncoderChoice::Auto, EncoderChoice::Named("libx264".into())] {
            let mut j = job("libx264", WireFormat::Nv12, container);
            j.encoder = choice;
            for crf in [None, Some(18)] {
                let configured = j
                    .clone()
                    .with_quality(VideoQuality {
                        crf,
                        ..VideoQuality::default()
                    })
                    .unwrap();
                let argv = encode_argv(&configured, Path::new("/out.mp4")).unwrap();
                assert_eq!(formats(&argv), ["nv12", "yuv420p"]);
            }
        }
        for name in ["libx264", "libx265", "hevc_nvenc", "hevc_videotoolbox"] {
            for (wire, input, output) in [
                (WireFormat::Rgba8, "rgba", "yuv420p"),
                (WireFormat::Bgra8, "bgra", "yuv420p"),
                (WireFormat::P010, "p010le", "yuv420p10le"),
            ] {
                let argv = encode_argv(&job(name, wire, container), Path::new("/out.mp4")).unwrap();
                assert_eq!(formats(&argv), [input, output]);
            }
        }
    }
}

#[test]
fn alpha_gif_and_quality_refusals_are_not_bypassed() {
    let argv = encode_argv(
        &job("qtrle", WireFormat::Bgra8, Container::MovTransparent),
        Path::new("/alpha.mov"),
    )
    .unwrap();
    assert_eq!(formats(&argv), ["bgra", "argb"]);
    let mut gif = job("unused", WireFormat::Nv12, Container::Gif);
    gif.encoder = EncoderChoice::Auto;
    let argv = encode_argv(&gif, Path::new("/out.gif")).unwrap();
    assert_eq!(formats(&argv), ["nv12"]);
    assert!(argv.windows(2).any(|pair| pair == ["-f", "gif"]));
    for name in ["h264_nvenc", "hevc_videotoolbox"] {
        assert!(
            job(name, WireFormat::Nv12, Container::Mp4)
                .with_quality(VideoQuality {
                    crf: Some(18),
                    ..VideoQuality::default()
                })
                .is_err()
        );
        assert!(
            encode_argv(
                &job(name, WireFormat::Nv12, Container::MovTransparent),
                Path::new("/alpha.mov"),
            )
            .is_err()
        );
    }
}

#[test]
fn encoder_capabilities_parses_and_probes_supported_pixel_formats() {
    use fmn_output::EncoderCapabilities;

    let sample = "\
Encoder h264_nvenc [NVIDIA NVENC H.264 encoder]:
    General capabilities: dr1 delay hardware
    Threading capabilities: none
    Supported hardware devices: cuda cuda
    Supported pixel formats: yuv420p nv12 p010le yuv444p p016le cuda
";
    let parsed = EncoderCapabilities::parse_pixel_formats(sample);
    assert!(parsed.contains("nv12"));
    assert!(parsed.contains("p010le"));
    assert!(parsed.contains("yuv420p"));

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        use std::sync::Arc;
        use std::time::Duration;

        use fmn_output::FfmpegTool;
        use fmn_platform::process::{
            FfmpegLocator, FfmpegLocatorError, ProcessRunner, ProcessSpec, StdFfmpegLocator,
            StdProcessRunner,
        };

        let executable =
            match StdFfmpegLocator::from_host_path().locate_ffmpeg(Path::new("ffmpeg")) {
                Ok(executable) => executable,
                Err(FfmpegLocatorError::NotFound) => return,
                Err(error) => panic!("ffmpeg locator refused: {error}"),
            };
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = Path::new("/tmp").canonicalize().unwrap().join(format!(
            "fmn-nv12-probe-{}-{unique}",
            std::process::id()
        ));
        std::fs::create_dir(&root).unwrap();
        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();
        let runner = Arc::new(StdProcessRunner);
        let tool = FfmpegTool::resolve(executable, runner.as_ref(), &root).unwrap();
        let program = tool.path().to_path_buf();
        let caps = EncoderCapabilities::probe(&tool, runner.as_ref()).unwrap();
        if caps.offers("libx264") {
            let fmts = caps
                .probe_pixel_formats(&tool, runner.as_ref(), "libx264")
                .unwrap();
            assert!(fmts.contains("nv12") && fmts.contains("yuv420p"));
        }
        if caps.offers("h264_nvenc") {
            let fmts = caps
                .probe_pixel_formats(&tool, runner.as_ref(), "h264_nvenc")
                .unwrap();
            assert!(fmts.contains("nv12") && fmts.contains("p010le"));
        }
        // Verify decoded framemd5 identity when feeding rawvideo nv12 with vs without
        // a redundant output -pix_fmt yuv420p conversion.
        let mut nv12_bytes = vec![128u8; 64 * 36 * 3 / 2];
        for (i, b) in nv12_bytes[..64 * 36].iter_mut().enumerate() {
            *b = (16 + (i * 7) % 219) as u8;
        }
        let run_md5 = |extra_pix_fmt: bool| {
            let mut argv = vec![
                "-v".into(),
                "error".into(),
                "-f".into(),
                "rawvideo".into(),
                "-pix_fmt".into(),
                "nv12".into(),
                "-video_size".into(),
                "64x36".into(),
                "-framerate".into(),
                "30/1".into(),
                "-i".into(),
                "-".into(),
            ];
            if extra_pix_fmt {
                argv.extend(["-pix_fmt".into(), "yuv420p".into()]);
            }
            argv.extend(["-f".into(), "framemd5".into(), "-".into()]);
            let out = runner
                .run(&ProcessSpec {
                    program: program.clone(),
                    argv,
                    env: vec![
                        ("LANG".into(), "C".into()),
                        ("LC_ALL".into(), "C".into()),
                        ("TMPDIR".into(), root.display().to_string()),
                    ],
                    cwd: None,
                    stdin: Some(nv12_bytes.clone()),
                    timeout: Duration::from_secs(15),
                    max_output_bytes: 1 << 20,
                })
                .unwrap();
            assert!(out.success(), "{}", String::from_utf8_lossy(&out.stderr));
            out.stdout
        };
        let passthrough = String::from_utf8(run_md5(false)).unwrap();
        assert!(passthrough.contains("0,          0,          0,        1"));
    }
}

