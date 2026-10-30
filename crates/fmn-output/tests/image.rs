//! Raster ingestion through the same bounded capability used by front doors.

use fmn_codec::{CompressionLevel, encode_rgba8};
use fmn_output::{ImageDecodeError, ImageDecodeLimits, ImageDecoder, ImageInputFormat};

const PIXELS: [u8; 16] = [
    255, 0, 0, 255, 0, 255, 0, 128, 0, 0, 255, 64, 255, 255, 0, 0,
];

fn png() -> Vec<u8> {
    encode_rgba8(2, 2, &PIXELS, CompressionLevel::Fast)
}

#[test]
fn native_images_keep_orientation_alpha_and_identity_without_ffmpeg() {
    let mut decoder =
        ImageDecoder::new(ImageDecodeLimits::default(), Some("/missing/ffmpeg".into())).unwrap();
    let source = png();
    let first = decoder.decode(&source).unwrap();
    assert_eq!((first.image.width, first.image.height), (2, 2));
    assert_eq!(first.image.rgba, PIXELS);
    assert_eq!(first.report.source_digest, fmn_hash::sha256(&source));
    assert!(first.report.invocation.is_none());
    // Harmless container metadata changes the source but not its decoded image.
    let mut source = source.clone();
    let payload = b"note\0same pixels";
    let mut chunk = b"tEXt".to_vec();
    chunk.extend_from_slice(payload);
    let mut encoded_chunk = (payload.len() as u32).to_be_bytes().to_vec();
    encoded_chunk.extend_from_slice(&chunk);
    encoded_chunk.extend_from_slice(&fmn_codec::checksum::crc32(&chunk).to_be_bytes());
    source.splice(source.len() - 12..source.len() - 12, encoded_chunk);
    let second = decoder.decode(&source).unwrap();
    assert_ne!(first.report.source_digest, second.report.source_digest);
    assert_eq!(first.report.pixel_digest, second.report.pixel_digest);
    let different_shape = encode_rgba8(4, 1, &PIXELS, CompressionLevel::Fast);
    assert_ne!(
        first.report.pixel_digest,
        decoder
            .decode(&different_shape)
            .unwrap()
            .report
            .pixel_digest
    );
}

#[test]
fn native_only_formats_name_the_capability_and_unknown_inputs_never_probe() {
    let mut decoder = ImageDecoder::new(ImageDecodeLimits::default(), None).unwrap();
    for (bytes, format) in [
        (b"GIF89a\x02\x00\x02\x00".as_slice(), ImageInputFormat::Gif),
        (b"BM fixture".as_slice(), ImageInputFormat::Bmp),
        (
            b"RIFF\x04\x00\x00\x00WEBP".as_slice(),
            ImageInputFormat::WebP,
        ),
        (
            b"II\x2a\x00\x08\x00\x00\x00".as_slice(),
            ImageInputFormat::Tiff,
        ),
    ] {
        let error = decoder.decode(bytes).unwrap_err();
        assert!(error.to_string().contains("PNG or JPEG"));
        assert!(
            matches!(error, ImageDecodeError::TranscoderRequired { format: actual } if actual == format)
        );
    }
    let mut configured =
        ImageDecoder::new(ImageDecodeLimits::default(), Some("/missing/ffmpeg".into())).unwrap();
    for bytes in [
        b"https://example.invalid/picture.png".as_slice(),
        b"ffconcat version 1.0\nfile '/private/image.png'",
        b"#EXTM3U\nimage.png",
        b"<svg/>",
        b"RIFF....WAVE",
    ] {
        assert!(matches!(
            configured.decode(bytes),
            Err(ImageDecodeError::UnsupportedInput)
        ));
    }
}

#[test]
fn damaged_native_images_and_decode_budgets_never_fall_through() {
    let mut decoder = ImageDecoder::new(
        ImageDecodeLimits {
            max_input_bytes: 4096,
            max_pixels: 3,
            max_chunks: 4096,
        },
        Some("/missing/ffmpeg".into()),
    )
    .unwrap();
    assert!(matches!(
        decoder.decode(&png()),
        Err(ImageDecodeError::NativePng(_))
    ));
    assert!(matches!(
        decoder.decode(b"\x89PNG\r\n\x1a\n"),
        Err(ImageDecodeError::NativePng(_))
    ));
    assert!(matches!(
        decoder.decode(b"\xff\xd8\xff"),
        Err(ImageDecodeError::NativeJpeg(_))
    ));
    let mut decoder = ImageDecoder::new(
        ImageDecodeLimits {
            max_input_bytes: 8,
            ..ImageDecodeLimits::default()
        },
        Some("/missing/ffmpeg".into()),
    )
    .unwrap();
    assert!(matches!(
        decoder.decode(&png()),
        Err(ImageDecodeError::InputOversized { .. })
    ));
    for limits in [
        ImageDecodeLimits {
            max_pixels: 0,
            ..ImageDecodeLimits::default()
        },
        ImageDecodeLimits {
            max_input_bytes: 0,
            ..ImageDecodeLimits::default()
        },
        ImageDecodeLimits {
            max_chunks: 0,
            ..ImageDecodeLimits::default()
        },
        ImageDecodeLimits {
            max_pixels: u64::MAX,
            ..ImageDecodeLimits::default()
        },
    ] {
        assert!(matches!(
            ImageDecoder::new(limits, None),
            Err(ImageDecodeError::InvalidLimits)
        ));
    }
}

#[test]
fn invalid_process_policy_is_refused_before_discovery_and_native_stays_independent() {
    use fmn_output::JobLimits;
    let decoder =
        ImageDecoder::new(ImageDecodeLimits::default(), Some("/missing/ffmpeg".into())).unwrap();
    assert!(matches!(
        decoder.with_job_limits(JobLimits {
            timeout: std::time::Duration::ZERO,
            ..JobLimits::default()
        }),
        Err(ImageDecodeError::InvalidJobLimits(_))
    ));
    let mut decoder =
        ImageDecoder::new(ImageDecodeLimits::default(), Some("/missing/ffmpeg".into()))
            .unwrap()
            .with_job_limits(JobLimits {
                max_artifact_bytes: 1,
                ..JobLimits::default()
            })
            .unwrap();
    assert_eq!(decoder.decode(&png()).unwrap().image.rgba, PIXELS);
    assert!(matches!(
        decoder.decode(b"BM fixture"),
        Err(ImageDecodeError::Capability(_))
    ));
}

#[cfg(all(not(target_arch = "wasm32"), feature = "exact-process"))]
#[test]
fn real_raster_formats_keep_pixels_and_animation_selects_only_the_first_frame() {
    let executable: std::path::PathBuf = match std::env::var_os("FMN_IMAGE_FFMPEG") {
        Some(path) => path.into(),
        None if std::env::var_os("FMN_REQUIRE_FFMPEG").is_some() => "ffmpeg".into(),
        None => {
            eprintln!("real image decode: set FMN_IMAGE_FFMPEG or FMN_REQUIRE_FFMPEG");
            return;
        }
    };
    // Independent 2x2 fixtures encoded by Pillow. The GIF has a second, black
    // frame: decoding it as a sequence would change the expected raster.
    const FIXTURES: [(ImageInputFormat, &str); 4] = [
        (
            ImageInputFormat::Bmp,
            "424d460000000000000036000000280000000200000002000000010020000000000010000000c40e0000c40e00000000000000000000ff0000ff00ffffff0000ffff00ff00ff",
        ),
        (
            ImageInputFormat::Gif,
            "474946383961020002008100000000ff00ff00ff0000ffff0021ff0b4e45545343415045322e30030100000021f904000a0000002c0000000002000200000807000504003020200021f904010a0001002c0000000002000200810000000000000000000000000806000108041010003b",
        ),
        (
            ImageInputFormat::WebP,
            "524946462c000000574542505650384c1f0000002f014000001f201048da1f7a8df9171014f93fdafc075f24e0070811fd0f0100",
        ),
        (
            ImageInputFormat::Tiff,
            "49492a00080000000b0000010400010000000200000001010400010000000200000002010300040000009200000003010300010000000100000006010300010000000200000011010400010000009a0000001501030001000000040000001601040001000000020000001701040001000000100000001c0103000100000001000000520103000100000002000000000000000800080008000800ff0000ff00ff00ff0000ffffffff00ff",
        ),
    ];
    let mut decoder =
        ImageDecoder::new(ImageDecodeLimits::default(), Some(executable.clone())).unwrap();
    let expected = [
        255, 0, 0, 255, 0, 255, 0, 255, 0, 0, 255, 255, 255, 255, 0, 255,
    ];
    let mut pixel_digest = None;
    for (format, hex) in FIXTURES {
        let bytes: Vec<u8> = hex
            .as_bytes()
            .as_chunks::<2>()
            .0
            .iter()
            .map(|pair| u8::from_str_radix(std::str::from_utf8(pair).unwrap(), 16).unwrap())
            .collect();
        let decoded = decoder
            .decode(&bytes)
            .unwrap_or_else(|error| panic!("{}: {error}", format.name()));
        assert_eq!((decoded.image.width, decoded.image.height), (2, 2));
        assert_eq!(
            decoded.image.rgba,
            expected,
            "{} changed color, alpha or orientation",
            format.name()
        );
        assert_eq!(decoded.report.format, format);
        assert_eq!(decoded.report.source_digest, fmn_hash::sha256(&bytes));
        if let Some(expected) = pixel_digest {
            assert_eq!(decoded.report.pixel_digest, expected);
        }
        pixel_digest = Some(decoded.report.pixel_digest);
        let invocation = decoded.report.invocation.unwrap();
        assert_eq!(invocation.provenance.encoder.as_deref(), Some("png"));
        assert!(!invocation.artifact.exists());
        let mut small = ImageDecoder::new(
            ImageDecodeLimits {
                max_pixels: 3,
                ..ImageDecodeLimits::default()
            },
            Some(executable.clone()),
        )
        .unwrap();
        assert!(
            small.decode(&bytes).is_err(),
            "{} ignored the pixel budget",
            format.name()
        );
    }
}

#[cfg(all(not(target_arch = "wasm32"), not(feature = "exact-process")))]
#[test]
fn a_native_only_build_names_the_missing_runner_without_discovering_it() {
    let mut decoder =
        ImageDecoder::new(ImageDecodeLimits::default(), Some("/missing/ffmpeg".into())).unwrap();
    let error = decoder.decode(b"BM fixture").unwrap_err();
    assert!(error.to_string().contains("exact-process"));
    assert_eq!(decoder.decode(&png()).unwrap().image.rgba, PIXELS);
}

#[cfg(all(not(target_arch = "wasm32"), feature = "ffmpeg-test-fixture"))]
mod boundary {
    use super::*;
    use fmn_output::{Boundary, BoundaryError, FfmpegTool, JobLimits};
    use fmn_platform::process::{FfmpegLocator, StdFfmpegLocator, StdProcessRunner};
    use std::path::PathBuf;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static NEXT: AtomicUsize = AtomicUsize::new(0);

    fn decoder(mode: &str, max_artifact_bytes: u64) -> ImageDecoder {
        let root = std::env::temp_dir().join(format!(
            "fmn-image-decode-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&root).unwrap();
        std::fs::write(root.join(".fmn-native-ffmpeg-mode"), mode).unwrap();
        let executable = PathBuf::from(env!("CARGO_BIN_EXE_fmn-ffmpeg-test-fixture"));
        let executable = StdFfmpegLocator::default()
            .locate_ffmpeg(&executable)
            .unwrap();
        let runner = Arc::new(StdProcessRunner);
        let tool = FfmpegTool::resolve(executable, runner.as_ref(), &root).unwrap();
        let boundary = Boundary::new(
            tool,
            runner,
            JobLimits {
                max_artifact_bytes,
                ..JobLimits::default()
            },
            root,
        )
        .unwrap();
        ImageDecoder::with_boundary(ImageDecodeLimits::default(), boundary).unwrap()
    }

    #[test]
    fn image_boundary_returns_real_pixels_and_exact_input_and_decoder_receipts() {
        let mut decoder = decoder("image-decode", 1 << 20);
        let source = b"BM immutable fixture bytes";
        let decoded = decoder.decode(source).unwrap();
        assert_eq!(decoded.image.rgba, PIXELS);
        assert_eq!((decoded.image.width, decoded.image.height), (2, 2));
        assert_eq!(decoded.report.source_digest, fmn_hash::sha256(source));
        let native = ImageDecoder::new(ImageDecodeLimits::default(), None)
            .unwrap()
            .decode(&png())
            .unwrap();
        assert_eq!(decoded.report.pixel_digest, native.report.pixel_digest);
        let invocation = decoded.report.invocation.unwrap();
        assert_eq!(invocation.provenance.encoder.as_deref(), Some("png"));
        assert_eq!(invocation.provenance.tool_sha256_hex.len(), 64);
        assert!(
            invocation
                .provenance
                .argv
                .windows(2)
                .any(|v| v == ["-f", "bmp_pipe"])
        );
        assert!(
            invocation
                .provenance
                .argv
                .windows(2)
                .any(|v| v[0] == "-i" && v[1].ends_with("source.image"))
        );
        assert!(
            !invocation.artifact.exists(),
            "private decoded image escaped its job lifetime"
        );
        assert!(decoder.with_job_limits(JobLimits::default()).is_err());
    }

    #[test]
    fn image_boundary_rejects_source_substitution_damage_and_truncated_output() {
        assert!(matches!(
            decoder("image-decode-swap", 1 << 20).decode(b"BM fixture"),
            Err(ImageDecodeError::InvalidRaster(_))
        ));
        assert!(matches!(
            decoder("image-decode-corrupt", 1 << 20).decode(b"BM fixture"),
            Err(ImageDecodeError::NativePng(_))
        ));
        assert!(matches!(
            decoder("image-decode", 32).decode(b"BM fixture"),
            Err(ImageDecodeError::Boundary(
                BoundaryError::ArtifactOversized { .. }
            ))
        ));
    }
}
