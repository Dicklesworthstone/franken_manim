//! Input color metadata is part of the wire contract, not just output tags.
#![forbid(unsafe_code)]

use std::path::Path;

use fmn_frame::ColorRange;
use fmn_output::negotiate::{EncoderPreset, VideoQuality, encode_argv, encode_argv_with_formats};
use fmn_output::{
    ColorDescription, Container, EncoderChoice, Primaries, Transfer, VideoJob, WireFormat,
};

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
                assert_eq!(matrix[0].1, if wire.has_alpha() { "rgb" } else { "bt709" });
                assert_eq!(matrix[1].1, "bt709");
                assert!(
                    !argv
                        .iter()
                        .any(|arg| matches!(arg.as_str(), "-vf" | "-filter:v" | "-filter_complex"))
                );
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

#[test]
fn explicit_hdr_preserves_the_existing_sdr_arguments_byte_for_byte() {
    for (wire, input_format, output_format) in [
        (WireFormat::Nv12, "nv12", "yuv420p"),
        (WireFormat::P010, "p010le", "yuv420p10le"),
    ] {
        let sdr = job(
            wire,
            ColorDescription::for_native_wire(wire),
            Container::Mp4,
        );
        let expected = format!(
            "-hide_banner -loglevel error -f rawvideo -pix_fmt {input_format} -video_size 64x36 -framerate 30000/1001 -color_primaries bt709 -color_trc iec61966-2-1 -colorspace bt709 -color_range tv -i - -c:v libx264 -pix_fmt {output_format} -movflags +faststart -color_primaries bt709 -color_trc iec61966-2-1 -colorspace bt709 -color_range tv -y /out.mp4"
        );
        assert_eq!(
            encode_argv(&sdr, Path::new("/out.mp4")).unwrap().join(" "),
            expected
        );
    }
}

#[test]
fn hdr_pq_uses_software_hevc_and_describes_both_sides_of_the_boundary() {
    for container in [Container::Mp4, Container::Mov] {
        let hdr = job(
            WireFormat::Nv12,
            ColorDescription::for_native_wire(WireFormat::Nv12),
            container,
        )
        .with_hdr_pq()
        .unwrap();
        assert_eq!(hdr.wire, WireFormat::P010);
        assert_eq!(hdr.color, ColorDescription::hdr_pq());
        assert_eq!(hdr.encoder, EncoderChoice::Named("libx265".into()));
        assert_eq!(hdr.resolved_encoder().unwrap().as_deref(), Some("libx265"));

        // Direct literal construction has the same software default.
        let literal = VideoJob {
            encoder: EncoderChoice::Auto,
            ..hdr.clone()
        };
        assert_eq!(
            literal.resolved_encoder().unwrap().as_deref(),
            Some("libx265")
        );
        let argv = encode_argv(&hdr, Path::new("/hdr.mov")).unwrap();
        assert_eq!(argv, encode_argv(&literal, Path::new("/hdr.mov")).unwrap());
        let input = argv.iter().position(|arg| arg == "-i").unwrap();
        for (flag, expected) in [
            ("-color_primaries", "bt2020"),
            ("-color_trc", "smpte2084"),
            ("-colorspace", "bt2020nc"),
            ("-color_range", "tv"),
        ] {
            let entries = values(&argv, flag);
            assert_eq!(entries.len(), 2, "{flag}: {argv:?}");
            assert!(entries[0].0 < input && entries[1].0 > input + 1);
            assert_eq!(entries[0].1, expected);
            assert_eq!(entries[1].1, expected);
        }
        let formats = values(&argv, "-pix_fmt");
        assert_eq!(formats[0].1, "p010le");
        assert_eq!(formats[1].1, "yuv420p10le");
        assert!(
            !argv
                .iter()
                .any(|arg| matches!(arg.as_str(), "-vf" | "-filter:v" | "-filter_complex"))
        );
    }
}

#[test]
fn incompatible_pq_literals_cannot_bypass_the_hdr_profile() {
    let hdr = job(WireFormat::P010, ColorDescription::hdr_pq(), Container::Mp4);
    let incompatible = [
        VideoJob {
            color: ColorDescription {
                primaries: Primaries::Bt709,
                ..hdr.color
            },
            ..hdr.clone()
        },
        VideoJob {
            color: ColorDescription {
                range: ColorRange::Full,
                ..hdr.color
            },
            ..hdr.clone()
        },
        VideoJob {
            wire: WireFormat::Nv12,
            ..hdr.clone()
        },
        VideoJob {
            wire: WireFormat::Rgba8,
            ..hdr.clone()
        },
        VideoJob {
            wire: WireFormat::Bgra8,
            ..hdr.clone()
        },
        VideoJob {
            container: Container::MovTransparent,
            ..hdr.clone()
        },
        VideoJob {
            container: Container::Gif,
            ..hdr
        },
    ];
    for invalid in incompatible {
        assert_eq!(
            invalid.resolved_encoder().unwrap_err().0,
            "PQ HDR requires BT.2020 primaries, limited-range P010, and opaque MP4/MOV"
        );
        assert!(encode_argv(&invalid, Path::new("/invalid.mov")).is_err());
    }
    for container in [Container::Gif, Container::MovTransparent] {
        assert!(
            job(WireFormat::Rgba8, ColorDescription::srgb_full(), container)
                .with_hdr_pq()
                .unwrap_err()
                .0
                .contains("opaque MP4/MOV")
        );
    }
}

#[test]
fn hdr_pq_preserves_explicit_encoders_and_binds_quality_to_its_default() {
    let sdr = job(
        WireFormat::Nv12,
        ColorDescription::for_native_wire(WireFormat::Nv12),
        Container::Mp4,
    );
    let quality = VideoQuality {
        crf: Some(18),
        preset: Some(EncoderPreset::Fast),
        ..VideoQuality::default()
    };
    let hdr = sdr
        .clone()
        .with_hdr_pq()
        .unwrap()
        .with_quality(quality)
        .unwrap();
    assert_eq!(hdr.quality().unwrap(), quality);
    assert_eq!(hdr.resolved_encoder().unwrap().as_deref(), Some("libx265"));
    let argv = encode_argv(&hdr, Path::new("/hdr.mp4")).unwrap();
    assert_eq!(values(&argv, "-crf")[0].1, "18");
    assert_eq!(values(&argv, "-preset")[0].1, "fast");

    for encoder in [
        EncoderChoice::Named("hevc_nvenc".into()),
        EncoderChoice::Named("libx264".into()),
        EncoderChoice::Configured {
            name: "libx265".into(),
            quality,
        },
    ] {
        let hdr = VideoJob {
            encoder: encoder.clone(),
            ..sdr.clone()
        }
        .with_hdr_pq()
        .unwrap();
        assert_eq!(hdr.encoder, encoder);
        assert_eq!(hdr.color.transfer, Transfer::Pq);
    }
}

#[test]
fn hdr_pq_refuses_unadvertised_or_eight_bit_encoder_inputs() {
    let hdr = job(WireFormat::P010, ColorDescription::hdr_pq(), Container::Mp4);
    for formats in [vec![], vec!["yuv420p".into(), "nv12".into(), "rgba".into()]] {
        assert!(
            encode_argv_with_formats(&hdr, Path::new("/hdr.mp4"), &formats)
                .unwrap_err()
                .0
                .contains("10-bit")
        );
    }
    for format in ["p010le", "yuv420p10le", "yuv422p10le", "yuv444p10le"] {
        let argv =
            encode_argv_with_formats(&hdr, Path::new("/hdr.mp4"), &[format.to_owned()]).unwrap();
        assert_eq!(values(&argv, "-pix_fmt").last().unwrap().1, format);
    }
}

#[cfg(all(unix, feature = "exact-process"))]
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

    let executable = match StdFfmpegLocator::from_host_path().locate_ffmpeg(Path::new("ffmpeg")) {
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
    let root = Path::new("/tmp").canonicalize().unwrap().join(format!(
        "fmn-color-roundtrip-{}-{unique}",
        std::process::id()
    ));
    std::fs::create_dir(&root).unwrap();
    std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();
    let runner = Arc::new(StdProcessRunner);
    let tool = FfmpegTool::resolve(executable, runner.as_ref(), &root).unwrap();
    let program = tool.path().to_path_buf();
    let caps = EncoderCapabilities::probe(&tool, runner.as_ref()).unwrap();
    assert!(
        caps.offers("libx264"),
        "real color roundtrip requires libx264"
    );
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

#[test]
fn native_wire_metadata_describes_srgb_samples_without_retagging_external_bt709() {
    for wire in [
        WireFormat::Rgba8,
        WireFormat::Bgra8,
        WireFormat::Nv12,
        WireFormat::P010,
    ] {
        let color = ColorDescription::for_native_wire(wire);
        assert_eq!(color.transfer, fmn_output::Transfer::Srgb);
        assert_eq!(color.primaries, fmn_output::Primaries::Bt709);
        assert_eq!(
            color.range,
            if wire.has_alpha() {
                ColorRange::Full
            } else {
                ColorRange::Limited
            }
        );
    }
    // External callers may legitimately supply already-BT.709-transferred
    // samples. Their explicit description must not be globally rewritten.
    assert_eq!(
        ColorDescription::video_bt709().transfer,
        fmn_output::Transfer::Bt709
    );
}

#[cfg(all(unix, feature = "exact-process"))]
#[test]
fn real_ffmpeg_p010_hevc_retains_more_than_256_luma_levels_and_color_tags() {
    use std::collections::BTreeSet;
    use std::os::unix::fs::PermissionsExt;
    use std::sync::Arc;
    use std::time::Duration;

    use fmn_frame::convert::rgba16f_to_p010;
    use fmn_frame::half::f16_from_f32;
    use fmn_frame::transfer::srgb_decode;
    use fmn_frame::{ChromaSiting, FrameBuffer, FrameLayout, PixelFormat};
    use fmn_output::{Boundary, EncoderCapabilities, FfmpegTool, JobLimits};
    use fmn_platform::process::{
        FfmpegLocator, FfmpegLocatorError, ProcessRunner, ProcessSpec, StdFfmpegLocator,
        StdProcessRunner,
    };

    let executable = match StdFfmpegLocator::from_host_path().locate_ffmpeg(Path::new("ffmpeg")) {
        Ok(executable) => executable,
        Err(FfmpegLocatorError::NotFound) => {
            assert_ne!(std::env::var("FMN_REQUIRE_FFMPEG").as_deref(), Ok("1"));
            eprintln!("SKIP real P010 roundtrip: ffmpeg is not installed");
            return;
        }
        Err(error) => panic!("ffmpeg locator refused: {error}"),
    };
    let unique = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let root = Path::new("/tmp").canonicalize().unwrap().join(format!(
        "fmn-p010-roundtrip-{}-{unique}",
        std::process::id()
    ));
    std::fs::create_dir(&root).unwrap();
    std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();
    let runner = Arc::new(StdProcessRunner);
    let tool = FfmpegTool::resolve(executable, runner.as_ref(), &root).unwrap();
    let program = tool.path().to_path_buf();
    let caps = EncoderCapabilities::probe(&tool, runner.as_ref()).unwrap();
    if !caps.offers("libx265") {
        fmn_core::test_inputs::skip_or_fail(
            "fmn-output::real_ffmpeg_p010_hevc_retains_more_than_256_luma_levels_and_color_tags",
            "libx265 offered by the installed ffmpeg",
        );
        return;
    }

    let (width, height) = (512u32, 64u32);
    let mut src =
        FrameBuffer::new(FrameLayout::tight(PixelFormat::Rgba16F, width, height).unwrap());
    let src_stride = src.layout().stride(0);
    for x in 0..width as usize {
        let bits = f16_from_f32(srgb_decode(x as f64 / (width - 1) as f64) as f32);
        let bytes = bits.to_le_bytes();
        let alpha = 0x3c00u16.to_le_bytes();
        for y in 0..height as usize {
            let at = y * src_stride + x * 8;
            src.plane_mut(0)[at..at + 8].copy_from_slice(&[
                bytes[0], bytes[1], bytes[0], bytes[1], bytes[0], bytes[1], alpha[0], alpha[1],
            ]);
        }
    }
    let mut p010 = FrameBuffer::new(FrameLayout::tight(PixelFormat::P010, width, height).unwrap());
    rgba16f_to_p010(&src, &mut p010, ColorRange::Limited, ChromaSiting::Left).unwrap();

    let color = ColorDescription::for_native_wire(WireFormat::P010);
    let mut job = VideoJob {
        width,
        height,
        fps: (30, 1),
        wire: WireFormat::P010,
        color,
        container: Container::Mp4,
        encoder: EncoderChoice::Named("libx265".into()),
        crf: Some(0),
    };
    let argv = encode_argv(&job, Path::new("/out.mp4")).unwrap();
    assert!(argv.windows(2).any(|w| w == ["-color_trc", "iec61966-2-1"]));
    assert!(argv.windows(2).any(|w| w == ["-pix_fmt", "yuv420p10le"]));

    let boundary = Boundary::new(tool, runner.clone(), JobLimits::default(), root.clone()).unwrap();
    let output = root.join("p010-ramp.mp4");
    job.crf = Some(0);
    let report = boundary
        .encode(&job, p010.as_bytes().to_vec(), &caps, &output)
        .unwrap();
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
                "yuv420p10le".into(),
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
    let first_row = &decoded.stdout[..width as usize * 2];
    let levels: BTreeSet<u16> = first_row
        .as_chunks::<2>()
        .0
        .iter()
        .map(|pair| u16::from_le_bytes(*pair))
        .collect();
    assert!(
        levels.len() > 256,
        "decoded 10-bit P010 stream kept only {} distinct luma levels across 512 ramp steps",
        levels.len()
    );
}

#[cfg(all(unix, feature = "exact-process"))]
#[test]
fn real_ffmpeg_pq_hevc_keeps_hdr_tags_and_ten_bit_precision_after_audio_mux() {
    use std::collections::BTreeSet;
    use std::os::unix::fs::PermissionsExt;
    use std::sync::Arc;
    use std::time::Duration;

    use fmn_codec::{SampleFormat, encode_wav};
    use fmn_output::{Boundary, EncoderCapabilities, FfmpegTool, JobLimits};
    use fmn_platform::process::{
        FfmpegLocator, FfmpegLocatorError, ProcessRunner, ProcessSpec, StdFfmpegLocator,
        StdProcessRunner,
    };

    let executable = match StdFfmpegLocator::from_host_path().locate_ffmpeg(Path::new("ffmpeg")) {
        Ok(executable) => executable,
        Err(FfmpegLocatorError::NotFound) => {
            assert_ne!(std::env::var("FMN_REQUIRE_FFMPEG").as_deref(), Ok("1"));
            eprintln!("SKIP real PQ roundtrip: ffmpeg is not installed");
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
        .join(format!("fmn-pq-roundtrip-{}-{unique}", std::process::id()));
    std::fs::create_dir(&root).unwrap();
    std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();
    let runner = Arc::new(StdProcessRunner);
    let tool = FfmpegTool::resolve(executable, runner.as_ref(), &root).unwrap();
    let program = tool.path().to_path_buf();
    let caps = EncoderCapabilities::probe(&tool, runner.as_ref()).unwrap();
    if !caps.offers("libx265") {
        fmn_core::test_inputs::skip_or_fail(
            "fmn-output::real_ffmpeg_pq_hevc_keeps_hdr_tags_and_ten_bit_precision_after_audio_mux",
            "libx265 offered by the installed ffmpeg",
        );
        return;
    }

    // A neutral limited-range PQ ramp isolates the negotiated wire contract
    // from the native converter's independently tested color arithmetic.
    let (width, height) = (512u32, 64u32);
    let mut frame = Vec::new();
    for _ in 0..height {
        for x in 0..width {
            let code = (64 + 876 * x / (width - 1)) as u16;
            frame.extend_from_slice(&(code << 6).to_le_bytes());
        }
    }
    for _ in 0..width * height / 2 {
        frame.extend_from_slice(&(512u16 << 6).to_le_bytes());
    }
    let frames = frame.repeat(4);
    let hdr = VideoJob {
        width,
        height,
        fps: (30, 1),
        ..job(
            WireFormat::Nv12,
            ColorDescription::for_native_wire(WireFormat::Nv12),
            Container::Mp4,
        )
    }
    .with_hdr_pq()
    .unwrap()
    .with_quality(VideoQuality {
        crf: Some(0),
        preset: Some(EncoderPreset::UltraFast),
        ..VideoQuality::default()
    })
    .unwrap();
    let audio = root.join("track.wav");
    std::fs::write(
        &audio,
        encode_wav(1, 48_000, SampleFormat::S16, &[0.0; 6_400]),
    )
    .unwrap();
    let boundary = Boundary::new(tool, runner.clone(), JobLimits::default(), root.clone()).unwrap();
    let output = root.join("pq-with-audio.mp4");
    let report = boundary
        .encode_with_audio(&hdr, frames, &audio, &caps, &output)
        .unwrap();
    assert_eq!(report.invocations.len(), 2);
    assert!(
        report.invocations[1]
            .provenance
            .argv
            .windows(2)
            .any(|pair| pair == ["-c:v", "copy"])
    );

    // showinfo observes the decoded frame's tags, including after the final
    // stream-copy mux; asserting encode arguments alone would miss lost tags.
    let decoded = runner
        .run(&ProcessSpec {
            program,
            argv: vec![
                "-hide_banner".into(),
                "-loglevel".into(),
                "info".into(),
                "-i".into(),
                output.display().to_string(),
                "-an".into(),
                "-vf".into(),
                "showinfo".into(),
                "-pix_fmt".into(),
                "yuv420p10le".into(),
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
    let diagnostic = String::from_utf8_lossy(&decoded.stderr);
    assert!(decoded.success(), "{diagnostic}");
    assert!(diagnostic.contains("Audio: aac"), "{diagnostic}");
    for expected in [
        "fmt:yuv420p10le",
        "color_range:tv",
        "color_space:bt2020nc",
        "color_primaries:bt2020",
        "color_trc:smpte2084",
    ] {
        assert!(
            diagnostic
                .lines()
                .any(|line| line.contains("showinfo") && line.contains(expected)),
            "decoded HDR stream lost {expected}: {diagnostic}"
        );
    }
    assert_eq!(
        decoded.stdout.len(),
        WireFormat::P010.frame_bytes(width, height).unwrap() * 4
    );
    let levels: BTreeSet<u16> = decoded.stdout[..width as usize * 2]
        .as_chunks::<2>()
        .0
        .iter()
        .map(|pair| u16::from_le_bytes(*pair))
        .collect();
    assert!(
        levels.len() > 256,
        "muxed PQ stream kept only {} distinct luma levels",
        levels.len()
    );
    eprintln!("real PQ encode and mux witness: {}", output.display());
}
