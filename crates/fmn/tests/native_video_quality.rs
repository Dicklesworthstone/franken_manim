//! Native quality admission and actual x264 bitstream evidence.

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use fmn::prelude::*;
use fmn::rendering::{EncoderPreset, EncoderTune, VideoQuality};
use fmn_platform::fs::VirtualFs;
use fmn_platform::process::{
    FfmpegExecutable, FfmpegLocator, FfmpegLocatorError, StdProcessRunner,
};

struct CountingLocator(Arc<AtomicUsize>);

impl FfmpegLocator for CountingLocator {
    fn locate_ffmpeg(&self, _: &Path) -> Result<FfmpegExecutable, FfmpegLocatorError> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Err(FfmpegLocatorError::NotFound)
    }
}

struct MustNotRun;

impl SceneConstruct for MustNotRun {
    fn construct(&mut self, _: &mut Stage<'_>) -> fmn::Result<()> {
        panic!("quality admission must precede scene execution");
    }
}

fn options(format: RenderFormat) -> (RenderOptions, Arc<AtomicUsize>) {
    let calls = Arc::new(AtomicUsize::new(0));
    let mut options = RenderOptions::with_format("/movie", format).unwrap();
    options.config.camera.resolution = (64, 36);
    options.config.camera.fps = 10;
    options.config.render.threads = fmn_config::config::ThreadPolicy::Fixed(1);
    options.typeset_cache = false;
    options.ffmpeg = Some(FfmpegCapability {
        runner: Arc::new(StdProcessRunner),
        locator: Arc::new(CountingLocator(calls.clone())),
        workdir_root: std::env::temp_dir(),
    });
    (options, calls)
}

#[test]
fn incompatible_quality_refuses_before_tool_discovery_or_scene_work() {
    let cases = [
        (
            "libx264",
            VideoQuality {
                crf: Some(52),
                ..VideoQuality::default()
            },
        ),
        (
            "libx264",
            VideoQuality {
                bitrate: Some(0),
                ..VideoQuality::default()
            },
        ),
        (
            "libx264",
            VideoQuality {
                crf: Some(18),
                bitrate: Some(8_000_000),
                ..VideoQuality::default()
            },
        ),
        (
            "libx265",
            VideoQuality {
                tune: Some(EncoderTune::Film),
                ..VideoQuality::default()
            },
        ),
        (
            "h264_nvenc",
            VideoQuality {
                crf: Some(18),
                ..VideoQuality::default()
            },
        ),
        (
            "h264_videotoolbox",
            VideoQuality {
                preset: Some(EncoderPreset::Slow),
                ..VideoQuality::default()
            },
        ),
        (
            "qtrle",
            VideoQuality {
                bitrate: Some(8_000_000),
                ..VideoQuality::default()
            },
        ),
    ];
    for format in [RenderFormat::Mp4, RenderFormat::Mov] {
        for (codec, quality) in cases {
            let (mut options, calls) = options(format);
            options.config.file_writer.video_codec = codec.to_owned();
            options.video_quality = quality;
            let error =
                render_with_fs(&mut MustNotRun, options, Arc::new(VirtualFs::new())).unwrap_err();
            assert!(
                matches!(error, RenderError::Negotiation(_)),
                "{codec}: {error}"
            );
            assert_eq!(calls.load(Ordering::SeqCst), 0);
        }
    }
    let (mut options, calls) = options(RenderFormat::Mov);
    options.config.camera.background_opacity = 0.0;
    options.video_quality.crf = Some(16);
    let error = render_with_fs(&mut MustNotRun, options, Arc::new(VirtualFs::new())).unwrap_err();
    assert!(matches!(error, RenderError::Negotiation(_)), "{error}");
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}

#[test]
fn configured_video_quality_inherits_and_explicit_rate_modes_replace_it() {
    let (mut options, _) = options(RenderFormat::Mp4);
    options.config.file_writer.video_crf = Some(28);
    options.config.file_writer.video_preset = Some("slow".to_owned());
    options.config.file_writer.video_tune = Some("animation".to_owned());
    assert_eq!(
        options.effective_video_quality().unwrap(),
        VideoQuality {
            crf: Some(28),
            preset: Some(EncoderPreset::Slow),
            tune: Some(EncoderTune::Animation),
            bitrate: None,
        }
    );
    options.video_quality.bitrate = Some(2_000_000);
    options.video_quality.preset = Some(EncoderPreset::Fast);
    assert_eq!(
        options.effective_video_quality().unwrap(),
        VideoQuality {
            crf: None,
            preset: Some(EncoderPreset::Fast),
            tune: Some(EncoderTune::Animation),
            bitrate: Some(2_000_000),
        }
    );
    options.config.file_writer.video_crf = Some(u32::MAX);
    options.config.file_writer.video_bitrate = Some(3_000_000);
    options.config.file_writer.video_preset = Some("invalid-but-overridden".to_owned());
    options.video_quality = VideoQuality {
        crf: Some(0),
        preset: Some(EncoderPreset::Slow),
        ..VideoQuality::default()
    };
    assert_eq!(
        options.effective_video_quality().unwrap(),
        VideoQuality {
            crf: Some(0),
            preset: Some(EncoderPreset::Slow),
            tune: Some(EncoderTune::Animation),
            bitrate: None,
        }
    );
}

#[test]
fn invalid_configured_video_quality_refuses_before_tool_or_scene_work() {
    for (crf, preset, tune, bitrate) in [
        (Some(52), None, None, None),
        (Some(u32::MAX), None, None, None),
        (None, Some("arbitrary"), None, None),
        (None, None, Some("film,grain"), None),
        (None, None, None, Some(0)),
        (Some(16), None, None, Some(2_000_000)),
    ] {
        let (mut options, calls) = options(RenderFormat::Mp4);
        options.config.file_writer.video_crf = crf;
        options.config.file_writer.video_preset = preset.map(str::to_owned);
        options.config.file_writer.video_tune = tune.map(str::to_owned);
        options.config.file_writer.video_bitrate = bitrate;
        let error =
            render_with_fs(&mut MustNotRun, options, Arc::new(VirtualFs::new())).unwrap_err();
        assert!(matches!(error, RenderError::Negotiation(_)), "{error}");
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }
}

#[test]
fn video_quality_on_native_formats_is_not_silently_discarded() {
    for format in [
        RenderFormat::PngSequence,
        RenderFormat::Gif,
        RenderFormat::Y4m,
    ] {
        let (mut options, calls) = options(format);
        options.video_quality.crf = Some(16);
        let error =
            render_with_fs(&mut MustNotRun, options, Arc::new(VirtualFs::new())).unwrap_err();
        assert!(matches!(error, RenderError::InvalidOptions(_)), "{error}");
        assert!(error.to_string().contains("video_quality"));
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }
}

// Read the generated video-only MP4/MOV fixtures, whose muxer uses four-byte
// AVC NAL lengths. This is a bounded test reader, not a general media decoder.
// Inspect actual SEI payload type 5 inside mdat, not arbitrary file substrings.
fn x264_sei(bytes: &[u8]) -> Option<String> {
    let mut at = 0_usize;
    while at < bytes.len() {
        let header = bytes.get(at..at.checked_add(8)?)?;
        let size = u32::from_be_bytes(header[..4].try_into().ok()?);
        let (header_len, size) = if size == 1 {
            let wide = bytes.get(at.checked_add(8)?..at.checked_add(16)?)?;
            (
                16,
                usize::try_from(u64::from_be_bytes(wide.try_into().ok()?)).ok()?,
            )
        } else if size == 0 {
            (8, bytes.len().checked_sub(at)?)
        } else {
            (8, usize::try_from(size).ok()?)
        };
        if size < header_len {
            return None;
        }
        let end = at.checked_add(size)?;
        let body = bytes.get(at.checked_add(header_len)?..end)?;
        if &header[4..8] == b"mdat" {
            let mut cursor = 0_usize;
            while cursor < body.len() {
                let prefix = body.get(cursor..cursor.checked_add(4)?)?;
                let length = usize::try_from(u32::from_be_bytes(prefix.try_into().ok()?)).ok()?;
                cursor = cursor.checked_add(4)?;
                let end = cursor.checked_add(length)?;
                let nal = body.get(cursor..end)?;
                if let Some(text) = user_data_sei(nal) {
                    return Some(text);
                }
                cursor = end;
            }
        }
        at = end;
    }
    None
}

fn user_data_sei(nal: &[u8]) -> Option<String> {
    if nal.first()? & 0x1f != 6 {
        return None;
    }
    let mut rbsp = Vec::new();
    let mut zeros = 0;
    for &byte in &nal[1..] {
        if zeros == 2 && byte == 3 {
            zeros = 0;
            continue;
        }
        rbsp.push(byte);
        zeros = if byte == 0 { (zeros + 1).min(2) } else { 0 };
    }
    fn extended(bytes: &[u8], at: &mut usize) -> Option<usize> {
        let mut value = 0_usize;
        loop {
            let byte = *bytes.get(*at)?;
            *at = at.checked_add(1)?;
            value = value.checked_add(usize::from(byte))?;
            if byte != 255 {
                return Some(value);
            }
        }
    }
    let mut at = 0;
    while at < rbsp.len() {
        let kind = extended(&rbsp, &mut at)?;
        let size = extended(&rbsp, &mut at)?;
        let end = at.checked_add(size)?;
        let payload = rbsp.get(at..end)?;
        if kind == 5 && payload.len() >= 16 {
            // user_data_unregistered: 16 UUID bytes precede the encoder text.
            let text = &payload[16..];
            if text.starts_with(b"x264 - core ") {
                return Some(
                    std::str::from_utf8(text)
                        .ok()?
                        .trim_end_matches('\0')
                        .to_owned(),
                );
            }
        }
        at = end;
    }
    None
}

#[test]
fn bitstream_probe_reads_sei_not_metadata_and_refuses_truncation() {
    let text = b"x264 - core TEST - options: crf=16.0 subme=8 psy_rd=0.40:0.00\0";
    let mut payload = vec![0; 16];
    payload.extend_from_slice(text);
    let mut rbsp = vec![5, u8::try_from(payload.len()).unwrap()];
    rbsp.extend_from_slice(&payload);
    rbsp.push(0x80);
    let mut nal = vec![6];
    let mut zeros = 0;
    for byte in rbsp {
        if zeros == 2 && byte <= 3 {
            nal.push(3);
            zeros = 0;
        }
        nal.push(byte);
        zeros = if byte == 0 { zeros + 1 } else { 0 };
    }
    let mut body = u32::try_from(nal.len()).unwrap().to_be_bytes().to_vec();
    body.extend(nal);
    let mut movie = u32::try_from(body.len() + 8)
        .unwrap()
        .to_be_bytes()
        .to_vec();
    movie.extend_from_slice(b"mdat");
    movie.extend(body);
    let expected = std::str::from_utf8(text).unwrap().trim_end_matches('\0');
    assert_eq!(x264_sei(&movie).as_deref(), Some(expected));
    assert!(x264_sei(&movie[..movie.len() - 1]).is_none());
    movie[4..8].copy_from_slice(b"free");
    assert!(
        x264_sei(&movie).is_none(),
        "metadata text is not encoder SEI"
    );
    movie[4..8].copy_from_slice(b"mdat");
    movie[8..12].copy_from_slice(&u32::MAX.to_be_bytes());
    assert!(x264_sei(&movie).is_none());
}

#[cfg(unix)]
#[test]
fn native_mp4_mov_quality_survives_into_provenance_and_x264_sei() {
    use fmn_platform::fs::StdFs;
    use fmn_platform::process::StdFfmpegLocator;

    struct Picture;
    impl SceneConstruct for Picture {
        fn construct(&mut self, stage: &mut Stage<'_>) -> fmn::Result<()> {
            stage.add(Circle::new().radius(0.8).color(BLUE))?;
            stage.wait(0.2)?;
            Ok(())
        }
    }
    let locator = StdFfmpegLocator::from_host_path();
    if locator.locate_ffmpeg(Path::new("ffmpeg")).is_err() {
        fmn_core::test_inputs::skip_or_fail(
            "native_mp4_mov_quality_survives_into_provenance_and_x264_sei",
            "an ffmpeg executable offering libx264 on PATH",
        );
        return;
    }
    let unique = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let root =
        std::env::temp_dir().join(format!("fmn-video-quality-{}-{unique}", std::process::id()));
    // The root is the ffmpeg workdir root, which the boundary refuses when
    // group-writable: make it private whatever the host's umask.
    let mut private = std::fs::DirBuilder::new();
    #[cfg(unix)]
    std::os::unix::fs::DirBuilderExt::mode(&mut private, 0o700);
    private.create(&root).unwrap();
    let root = root.canonicalize().unwrap();
    let capability = FfmpegCapability {
        runner: Arc::new(StdProcessRunner),
        locator: Arc::new(locator),
        workdir_root: root.clone(),
    };
    for (format, extension) in [(RenderFormat::Mp4, "mp4"), (RenderFormat::Mov, "mov")] {
        for camera in [false, true] {
            for crf in [16, 28] {
                let (mut options, _) = options(format);
                options.output = root.join(format!("quality-{camera}-{crf}.{extension}"));
                options.ffmpeg = Some(capability.clone());
                let quality = VideoQuality {
                    crf: Some(crf),
                    preset: Some(EncoderPreset::Slow),
                    tune: Some(EncoderTune::Animation),
                    bitrate: None,
                };
                if crf == 16 {
                    // Exercise file defaults through the same real sink and
                    // bitstream assertions as explicit Rust API controls.
                    options.config.file_writer.video_crf = Some(u32::from(crf));
                    options.config.file_writer.video_preset = Some("slow".to_owned());
                    options.config.file_writer.video_tune = Some("animation".to_owned());
                } else {
                    options.video_quality = quality;
                }
                if camera {
                    options.camera = Some(options.camera_config().unwrap());
                }
                let report = render_with_fs(&mut Picture, options, Arc::new(StdFs)).unwrap();
                assert_eq!(report.artifact.video_quality, Some(quality));
                // Every runtime capture reaches the encoder, one per clock
                // frame (as native_video.rs asserts): a 0.2 s wait at 10 fps
                // is three clock frames, as a 0.2 s play is in native_render.rs.
                let clock_frames = u64::try_from(report.scene.time.frames()).unwrap();
                assert_eq!(report.artifact.frame_count, clock_frames);
                assert_eq!(clock_frames, 3);
                assert_eq!(report.artifact.ffmpeg.len(), 1);
                let provenance = &report.artifact.ffmpeg[0].provenance;
                assert_eq!(provenance.encoder.as_deref(), Some("libx264"));
                assert!(
                    provenance
                        .argv
                        .windows(2)
                        .any(|pair| pair == ["-preset", "slow"])
                );
                assert!(
                    provenance
                        .argv
                        .windows(2)
                        .any(|pair| pair == ["-tune", "animation"])
                );
                let bytes = std::fs::read(&report.artifact.path).unwrap();
                let sei = x264_sei(&bytes).expect("output contains x264 user-data SEI");
                assert!(
                    sei.split_whitespace()
                        .any(|word| word == format!("crf={crf}.0")),
                    "{sei}"
                );
                assert!(
                    sei.split_whitespace().any(|word| word == "subme=8"),
                    "slow preset: {sei}"
                );
                assert!(
                    sei.split_whitespace()
                        .any(|word| word.starts_with("psy_rd=0.40:")),
                    "animation tune: {sei}"
                );
                eprintln!("{}: {sei}", report.artifact.path.display());
            }
        }
    }
    // Retain movies and logged SEI evidence. No new encoder default is inferred.
}

#[cfg(unix)]
mod wire_precision {
    use super::*;
    use std::path::PathBuf;
    use std::time::Duration;

    use fmn_output::{
        Boundary, ColorDescription, Container, EncoderCapabilities, EncoderChoice, FfmpegTool,
        JobLimits, VideoJob, WireFormat,
    };
    use fmn_platform::fs::StdFs;
    use fmn_platform::process::{ProcessOutcome, ProcessRunner, ProcessSpec, StdFfmpegLocator};

    struct Host {
        root: PathBuf,
        tool: FfmpegTool,
        caps: EncoderCapabilities,
        runner: Arc<StdProcessRunner>,
        locator: Arc<StdFfmpegLocator>,
    }

    fn host(test: &str, encoder: &str) -> Option<Host> {
        let locator = Arc::new(StdFfmpegLocator::from_host_path());
        let executable = match locator.locate_ffmpeg(Path::new("ffmpeg")) {
            Ok(executable) => executable,
            Err(FfmpegLocatorError::NotFound) => {
                fmn_core::test_inputs::skip_or_fail(test, "an ffmpeg executable on PATH");
                assert_ne!(std::env::var("FMN_REQUIRE_FFMPEG").as_deref(), Ok("1"));
                return None;
            }
            Err(error) => panic!("ffmpeg locator refused: {error}"),
        };
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root =
            std::env::temp_dir().join(format!("fmn-wire-{test}-{}-{unique}", std::process::id()));
        let mut private = std::fs::DirBuilder::new();
        std::os::unix::fs::DirBuilderExt::mode(&mut private, 0o700);
        private.create(&root).unwrap();
        let root = root.canonicalize().unwrap();
        let runner = Arc::new(StdProcessRunner);
        let tool = FfmpegTool::resolve(executable, runner.as_ref(), &root).unwrap();
        let caps = EncoderCapabilities::probe(&tool, runner.as_ref()).unwrap();
        if !caps.offers(encoder) {
            fmn_core::test_inputs::skip_or_fail(test, "the selected ffmpeg software encoder");
            assert_ne!(std::env::var("FMN_REQUIRE_FFMPEG").as_deref(), Ok("1"));
            return None;
        }
        Some(Host {
            root,
            tool,
            caps,
            runner,
            locator,
        })
    }

    fn job(wire: WireFormat) -> VideoJob {
        VideoJob {
            width: 64,
            height: 36,
            fps: (10, 1),
            wire,
            color: ColorDescription::for_native_wire(wire),
            container: Container::Mp4,
            encoder: EncoderChoice::Named("libx264".to_owned()),
            crf: Some(0),
        }
    }

    fn invoke(host: &Host, argv: Vec<String>, stdin: Option<Vec<u8>>) -> ProcessOutcome {
        let outcome = host
            .runner
            .run(&ProcessSpec {
                program: host.tool.path().to_path_buf(),
                argv,
                env: vec![
                    ("LANG".into(), "C".into()),
                    ("LC_ALL".into(), "C".into()),
                    ("TMPDIR".into(), host.root.display().to_string()),
                ],
                cwd: None,
                stdin,
                timeout: Duration::from_secs(15),
                max_output_bytes: 1 << 20,
            })
            .unwrap();
        assert!(
            outcome.success(),
            "{}",
            String::from_utf8_lossy(&outcome.stderr)
        );
        outcome
    }

    fn decode(host: &Host, path: &Path, pixel_format: Option<&str>) -> Vec<u8> {
        let mut argv = vec![
            "-v".into(),
            "error".into(),
            "-i".into(),
            path.display().to_string(),
            "-map".into(),
            "0:v:0".into(),
            "-an".into(),
        ];
        if let Some(format) = pixel_format {
            argv.extend(["-pix_fmt".into(), format.into()]);
        }
        argv.extend(["-f".into(), "rawvideo".into(), "-".into()]);
        invoke(host, argv, None).stdout
    }

    fn boundary(host: &Host) -> Boundary {
        Boundary::new(
            host.tool.clone(),
            host.runner.clone(),
            JobLimits {
                timeout: Duration::from_secs(15),
                max_log_bytes: 1 << 20,
                max_artifact_bytes: 1 << 20,
                ..JobLimits::default()
            },
            host.root.clone(),
        )
        .unwrap()
    }

    fn output_pixel_format(argv: &[String]) -> &str {
        let at = argv.iter().rposition(|arg| arg == "-pix_fmt").unwrap();
        &argv[at + 1]
    }

    // Each frame has distinct luma and two distinct chroma planes. P010
    // contains all four low-bit residues, so an 8-bit roundtrip is observable.
    fn samples(wire: WireFormat) -> (Vec<u8>, Vec<u8>) {
        let mut interleaved = Vec::new();
        let mut planar = Vec::new();
        for frame in 0..3 {
            let ten_bit = wire == WireFormat::P010;
            let luma: Vec<u16> = (0..36)
                .flat_map(|y| {
                    (0..64).map(move |x| {
                        if ten_bit {
                            64 + (19 * x + 37 * y + 53 * frame) % 877
                        } else {
                            16 + (3 * x + 5 * y + 19 * frame) % 220
                        }
                    })
                })
                .collect();
            let cb: Vec<u16> = (0..18)
                .flat_map(|y| {
                    (0..32).map(move |x| {
                        if ten_bit {
                            64 + (29 * x + 41 * y + 71 * frame) % 897
                        } else {
                            16 + (5 * x + 3 * y + 13 * frame) % 225
                        }
                    })
                })
                .collect();
            let cr: Vec<u16> = (0..18)
                .flat_map(|y| {
                    (0..32).map(move |x| {
                        if ten_bit {
                            64 + (31 * x + 43 * y + 73 * frame) % 897
                        } else {
                            16 + (7 * x + 2 * y + 17 * frame) % 225
                        }
                    })
                })
                .collect();
            for value in luma.iter().chain(&cb).chain(&cr) {
                if ten_bit {
                    planar.extend_from_slice(&value.to_le_bytes());
                } else {
                    planar.push(u8::try_from(*value).unwrap());
                }
            }
            for value in &luma {
                if ten_bit {
                    interleaved.extend_from_slice(&(value << 6).to_le_bytes());
                } else {
                    interleaved.push(u8::try_from(*value).unwrap());
                }
            }
            for (u, v) in cb.iter().zip(&cr) {
                if ten_bit {
                    interleaved.extend_from_slice(&(u << 6).to_le_bytes());
                    interleaved.extend_from_slice(&(v << 6).to_le_bytes());
                } else {
                    interleaved
                        .extend_from_slice(&[u8::try_from(*u).unwrap(), u8::try_from(*v).unwrap()]);
                }
            }
        }
        (interleaved, planar)
    }

    #[test]
    fn native_nv12_passthrough_matches_planar_reconstruction_at_lossless_and_lossy_quality() {
        let test = "native_nv12_passthrough";
        let Some(host) = host(test, "libx264") else {
            return;
        };
        let mut job = job(WireFormat::Nv12);
        let formats = host
            .caps
            .require_video_job(&host.tool, host.runner.as_ref(), &job)
            .unwrap()
            .unwrap();
        if !formats.formats.iter().any(|format| format == "nv12") {
            fmn_core::test_inputs::skip_or_fail(test, "libx264 with advertised NV12 input");
            assert_ne!(std::env::var("FMN_REQUIRE_FFMPEG").as_deref(), Ok("1"));
            return;
        }
        let (wire, expected) = samples(WireFormat::Nv12);
        for crf in [0, 23] {
            job.crf = Some(crf);
            let output = host.root.join(format!("nv12-{crf}.mp4"));
            let report = boundary(&host)
                .encode(&job, wire.clone(), &host.caps, &output)
                .unwrap();
            assert_eq!(report.invocations.len(), 1);
            assert_eq!(
                output_pixel_format(&report.invocations[0].provenance.argv),
                "nv12"
            );

            // Force the prior planar path for an independent real encode of
            // the very same wire bytes, codec, color tags and quality.
            let planar = host.root.join(format!("planar-{crf}.mp4"));
            let argv = fmn_output::negotiate::encode_argv_with_formats(
                &job,
                &planar,
                &["yuv420p".to_owned()],
            )
            .unwrap();
            assert_eq!(output_pixel_format(&argv), "yuv420p");
            invoke(&host, argv, Some(wire.clone()));
            let actual = decode(&host, &output, Some("yuv420p"));
            let reference = decode(&host, &planar, Some("yuv420p"));
            assert_eq!(actual.len(), expected.len());
            assert_eq!(
                actual, reference,
                "NV12 changed decoded samples at CRF {crf}"
            );
            if crf == 0 {
                assert_eq!(actual, expected, "lossless NV12 altered native YUV codes");
            }
        }
        eprintln!("NV12 decoder witnesses retained in {}", host.root.display());
    }

    #[test]
    fn native_p010_preserves_ten_bit_stream_and_matches_planar_input() {
        let test = "native_p010_precision";
        let Some(host) = host(test, "libx264") else {
            return;
        };
        let job = job(WireFormat::P010);
        let formats = match host
            .caps
            .require_video_job(&host.tool, host.runner.as_ref(), &job)
        {
            Ok(Some(formats)) => formats,
            Ok(None) => panic!("P010 admission must discover encoder formats"),
            Err(fmn_output::BoundaryError::UnsupportedEncoderPixelFormat {
                requested,
                wire,
                ..
            }) => {
                assert_eq!(requested, "libx264");
                assert_eq!(wire, WireFormat::P010);
                fmn_core::test_inputs::skip_or_fail(test, "libx264 with a 10-bit input format");
                assert_ne!(std::env::var("FMN_REQUIRE_FFMPEG").as_deref(), Ok("1"));
                return;
            }
            Err(error) => panic!("P010 capability probe failed: {error}"),
        };
        assert!(formats.formats.iter().any(|format| format == "yuv420p10le"));
        let (wire, planar) = samples(WireFormat::P010);
        let output = host.root.join("p010.mp4");
        let report = boundary(&host)
            .encode(&job, wire, &host.caps, &output)
            .unwrap();
        assert_eq!(
            output_pixel_format(&report.invocations[0].provenance.argv),
            "yuv420p10le"
        );

        let reference = host.root.join("planar10.mp4");
        let mut argv = fmn_output::negotiate::encode_argv_with_formats(
            &job,
            &reference,
            &["yuv420p10le".to_owned()],
        )
        .unwrap();
        let input_format = argv.iter().position(|arg| arg == "-pix_fmt").unwrap();
        assert!(input_format < argv.iter().position(|arg| arg == "-i").unwrap());
        argv[input_format + 1] = "yuv420p10le".into();
        invoke(&host, argv, Some(planar.clone()));

        // Do not force a decoder pixel format: an accidentally 8-bit stream
        // must emit half as many bytes and fail, even if upconversion could
        // manufacture a superficially 10-bit rawvideo output.
        let actual = decode(&host, &output, None);
        assert_eq!(
            actual.len(),
            planar.len(),
            "encoded stream lost its 10-bit depth"
        );
        assert!(
            actual
                .chunks_exact(2)
                .any(|bytes| { u16::from_le_bytes(bytes.try_into().unwrap()) & 3 != 0 }),
            "decoded samples retain precision beyond an 8-bit grid"
        );
        assert_eq!(
            actual,
            decode(&host, &reference, None),
            "P010 packing changed the planar 10-bit encoder input"
        );
        // CRF 0 is not universally lossless in a 10-bit x264 build. Compare
        // identical decoded reconstructions, without promising source equality.
        eprintln!("P010 decoder witnesses retained in {}", host.root.display());
    }

    #[test]
    fn native_p010_refuses_eight_bit_encoder_before_scene_construction() {
        let Some(host) = host("native_p010_refusal", "mpeg4") else {
            return;
        };
        let (mut options, _) = options(RenderFormat::Mp4);
        options.output = host.root.join("must-not-exist.mp4");
        let output = options.output.clone();
        options.config.file_writer.video_codec = "mpeg4".to_owned();
        options.config.file_writer.pixel_format = "p010le".to_owned();
        options.ffmpeg = Some(FfmpegCapability {
            runner: host.runner,
            locator: host.locator,
            workdir_root: host.root,
        });
        options.ffmpeg_limits.timeout = Duration::from_secs(15);
        let error = render_with_fs(&mut MustNotRun, options, Arc::new(StdFs)).unwrap_err();
        assert!(matches!(error, RenderError::Ffmpeg(_)), "{error}");
        assert!(error.to_string().contains("10-bit"), "{error}");
        assert!(!output.exists(), "refused scene published a video");
    }
}
