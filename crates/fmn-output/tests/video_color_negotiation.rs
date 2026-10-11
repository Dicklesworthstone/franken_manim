//! Input color metadata is part of the wire contract, not just output tags.
#![forbid(unsafe_code)]

use std::path::Path;

use fmn_frame::ColorRange;
use fmn_output::negotiate::encode_argv;
use fmn_output::{ColorDescription, Container, EncoderChoice, VideoJob, WireFormat};

fn job(wire: WireFormat, color: ColorDescription, container: Container) -> VideoJob {
    VideoJob {
        width: 64,
        height: 36,
        fps: (30_000, 1_001),
        wire,
        color,
        container,
        encoder: EncoderChoice::Auto,
        crf: None,
    }
}

fn values<'a>(argv: &'a [String], name: &str) -> Vec<(usize, &'a str)> {
    argv.windows(2)
        .enumerate()
        .filter(|(_, pair)| pair[0] == name)
        .map(|(at, pair)| (at, pair[1].as_str()))
        .collect()
}

#[test]
fn every_wire_describes_its_color_on_both_sides_of_the_input_boundary() {
    for wire in [
        WireFormat::Rgba8,
        WireFormat::Bgra8,
        WireFormat::Nv12,
        WireFormat::P010,
    ] {
        for (color, transfer, range) in [
            (ColorDescription::video_bt709(), "bt709", "tv"),
            (ColorDescription::srgb_full(), "iec61966-2-1", "pc"),
            (
                ColorDescription {
                    range: ColorRange::Limited,
                    ..ColorDescription::srgb_full()
                },
                "iec61966-2-1",
                "tv",
            ),
        ] {
            for container in [Container::Mp4, Container::Mov] {
                let argv =
                    encode_argv(&job(wire, color, container), Path::new("/video.mov")).unwrap();
                let input = argv.iter().position(|arg| arg == "-i").unwrap();
                for (flag, expected) in [
                    ("-color_primaries", "bt709"),
                    ("-color_trc", transfer),
                    ("-color_range", range),
                ] {
                    let entries = values(&argv, flag);
                    assert_eq!(entries.len(), 2, "{flag}: {argv:?}");
                    assert!(entries[0].0 < input && entries[1].0 > input + 1);
                    assert_eq!(entries[0].1, expected);
                    assert_eq!(entries[1].1, expected);
                }
                let matrix = values(&argv, "-colorspace");
                assert_eq!(matrix.len(), 2);
                assert!(matrix[0].0 < input && matrix[1].0 > input + 1);
                assert_eq!(
                    matrix[0].1,
                    if wire.has_alpha() { "rgb" } else { "bt709" }
                );
                assert_eq!(matrix[1].1, "bt709");
                assert!(!argv.iter().any(|arg| matches!(
                    arg.as_str(),
                    "-vf" | "-filter:v" | "-filter_complex"
                )));
            }
        }
    }
}

#[test]
fn transparent_rgb_keeps_an_identity_matrix_instead_of_a_yuv_matrix_tag() {
    for wire in [WireFormat::Rgba8, WireFormat::Bgra8] {
        for container in [Container::MovTransparent, Container::Gif] {
            let argv = encode_argv(
                &job(wire, ColorDescription::srgb_full(), container),
                Path::new("/alpha.mov"),
            )
            .unwrap();
            assert!(
                values(&argv, "-colorspace")
                    .iter()
                    .all(|(_, value)| *value == "rgb")
            );
            assert!(
                values(&argv, "-color_range")
                    .iter()
                    .all(|(_, value)| *value == "pc")
            );
        }
    }
}

#[cfg(unix)]
#[test]
fn real_ffmpeg_lossless_nv12_preserves_native_codes_without_a_hidden_matrix_transform() {
    use std::os::unix::fs::PermissionsExt;
    use std::sync::Arc;
    use std::time::Duration;

    use fmn_output::{Boundary, EncoderCapabilities, FfmpegTool, JobLimits};
    use fmn_platform::process::{
        FfmpegLocator, FfmpegLocatorError, ProcessRunner, ProcessSpec, StdFfmpegLocator,
        StdProcessRunner,
    };

    let executable = match StdFfmpegLocator::default().locate_ffmpeg(Path::new("ffmpeg")) {
        Ok(executable) => executable,
        Err(FfmpegLocatorError::NotFound) => {
            assert_ne!(std::env::var("FMN_REQUIRE_FFMPEG").as_deref(), Ok("1"));
            eprintln!("SKIP real color roundtrip: ffmpeg is not installed");
            return;
        }
        Err(error) => panic!("ffmpeg locator refused: {error}"),
    };
    let unique = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let root = Path::new("/tmp")
        .canonicalize()
        .unwrap()
        .join(format!("fmn-color-roundtrip-{}-{unique}", std::process::id()));
    std::fs::create_dir(&root).unwrap();
    std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();
    let runner = Arc::new(StdProcessRunner);
    let tool = FfmpegTool::resolve(executable, runner.as_ref(), &root).unwrap();
    let program = tool.path().to_path_buf();
    let caps = EncoderCapabilities::probe(&tool, runner.as_ref()).unwrap();
    assert!(caps.offers("libx264"), "real color roundtrip requires libx264");
    let boundary = Boundary::new(tool, runner.clone(), JobLimits::default(), root.clone()).unwrap();
    let color = ColorDescription {
        range: ColorRange::Limited,
        ..ColorDescription::srgb_full()
    };
    let mut job = job(WireFormat::Nv12, color, Container::Mp4);
    job.crf = Some(0);
    let (width, height) = (job.width as usize, job.height as usize);
    let mut wire = Vec::new();
    let mut expected = Vec::new();
    for frame in 0..4 {
        for y in 0..height {
            for x in 0..width {
                let sample = (16 + (3 * x + 5 * y + 19 * frame) % 220) as u8;
                wire.push(sample);
                expected.push(sample);
            }
        }
        let mut cb = Vec::new();
        let mut cr = Vec::new();
        for y in 0..height / 2 {
            for x in 0..width / 2 {
                let u = (16 + (5 * x + 3 * y + 13 * frame) % 225) as u8;
                let v = (16 + (7 * x + 2 * y + 17 * frame) % 225) as u8;
                wire.extend_from_slice(&[u, v]);
                cb.push(u);
                cr.push(v);
            }
        }
        expected.extend(cb);
        expected.extend(cr);
    }
    let output = root.join("native-colors.mp4");
    let report = boundary.encode(&job, wire, &caps, &output).unwrap();
    assert_eq!(report.invocations.len(), 1);
    let decoded = runner
        .run(&ProcessSpec {
            program,
            argv: vec![
                "-v".into(),
                "error".into(),
                "-i".into(),
                output.display().to_string(),
                "-pix_fmt".into(),
                "yuv420p".into(),
                "-f".into(),
                "rawvideo".into(),
                "-".into(),
            ],
            env: vec![
                ("LANG".into(), "C".into()),
                ("LC_ALL".into(), "C".into()),
                ("TMPDIR".into(), root.display().to_string()),
            ],
            cwd: None,
            stdin: None,
            timeout: Duration::from_secs(30),
            max_output_bytes: 1 << 20,
        })
        .unwrap();
    assert!(
        decoded.success(),
        "{}",
        String::from_utf8_lossy(&decoded.stderr)
    );
    assert_eq!(decoded.stdout.len(), expected.len());
    assert_eq!(
        decoded.stdout, expected,
        "native YUV codes were transformed before lossless encode"
    );
    // Retain the small encoded witness; do not delete shared or user files.
    eprintln!("real color roundtrip witness: {}", output.display());
}
