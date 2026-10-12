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
