//! Native author-facing raster preparation through the actual camera renderer.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use fmn::mobject::{ImageColorSpace, Mobject};
use fmn::prelude::*;
use fmn_codec::{CompressionLevel, PngLimits, decode_png, encode_rgba8};
use fmn_platform::fs::{FileSystem, FsError, FsNodeKind, VirtualFs};
use fmn_platform::process::{
    FfmpegExecutable, FfmpegLocator, FfmpegLocatorError, NoProcessRunner, StdFfmpegLocator,
    StdProcessRunner,
};

const PIXELS: [u8; 16] = [
    255, 0, 0, 255, 0, 255, 0, 128, 0, 0, 255, 255, 255, 255, 0, 0,
];

fn png() -> Vec<u8> {
    encode_rgba8(2, 2, &PIXELS, CompressionLevel::Fast)
}

struct Picture(ImageMobject);

impl SceneConstruct for Picture {
    fn construct(&mut self, stage: &mut Stage<'_>) -> fmn::Result<()> {
        stage.add(self.0.clone().with_height(3.0))?;
        Ok(())
    }
}

fn render_image(fs: Arc<VirtualFs>, image: ImageMobject, path: &str) -> Vec<u8> {
    render_image_with_mode(fs, image, path, DeterminismMode::Certified)
}

fn render_image_with_mode(
    fs: Arc<VirtualFs>,
    image: ImageMobject,
    path: &str,
    determinism: DeterminismMode,
) -> Vec<u8> {
    let mut options = RenderOptions::with_format(path, RenderFormat::Png).unwrap();
    options.config.camera.resolution = (96, 64);
    options.config.camera.background_color = "#000000".into();
    options.config.sizes.frame_height = 4.0;
    options.config.render.threads = ThreadPolicy::Fixed(1);
    options.config.determinism.mode = determinism;
    options.typeset_cache = false;
    options.camera = Some(options.camera_config().unwrap());
    let report = render_with_fs(&mut Picture(image), options, fs.clone()).unwrap();
    assert_eq!(report.artifact.frame_count, 1);
    let bytes = fs.read(Path::new(path)).unwrap();
    decode_png(&bytes, &PngLimits::default()).unwrap().rgba
}

fn pixel(image: &[u8], x: usize, y: usize) -> &[u8] {
    &image[(y * 96 + x) * 4..(y * 96 + x + 1) * 4]
}

#[test]
fn prepared_file_retains_pixels_receipt_and_shared_storage_after_source_changes() {
    let fs = Arc::new(VirtualFs::new());
    let source = png();
    // Detection follows bytes, not a trusted extension.
    fs.insert("/asset.data", source.clone());
    let mut loader = ImageLoader::new(ImageLoadOptions {
        determinism: DeterminismMode::Certified,
        ..ImageLoadOptions::default()
    })
    .unwrap();
    let prepared = loader.load_with_fs("/asset.data", fs.as_ref()).unwrap();
    let from_bytes = loader.decode(&source).unwrap();
    assert_eq!(
        prepared.report().source_path(),
        Some(Path::new("/asset.data"))
    );
    assert_eq!(prepared.report().source_bytes(), source.len() as u64);
    assert_eq!(prepared.report().decode().format, ImageInputFormat::Png);
    assert!(prepared.report().decode().invocation.is_none());
    assert_eq!(
        prepared.report().decode().source_digest,
        from_bytes.report().decode().source_digest,
    );
    assert_eq!(
        prepared.report().decode().pixel_digest,
        from_bytes.report().decode().pixel_digest,
    );
    assert!(from_bytes.report().source_path().is_none());
    assert_eq!(prepared.mobject().pixels(), PIXELS);
    let first: Mobject = prepared.mobject().into();
    let second: Mobject = prepared.clone().mobject().with_opacity(0.5).into();
    assert!(Arc::ptr_eq(
        &first.image.unwrap().shared_pixels(),
        &second.image.unwrap().shared_pixels(),
    ));

    fs.insert("/asset.data", b"source replaced after preparation".to_vec());
    let actual = render_image(fs.clone(), prepared.mobject(), "/prepared.png");
    let expected = render_image(
        fs.clone(),
        ImageMobject::from_rgba8(2, 2, PIXELS.to_vec()).unwrap(),
        "/expected.png",
    );
    assert_eq!(actual, expected);
    assert_eq!(pixel(&actual, 28, 12), [255, 0, 0, 255]);
    assert_eq!(pixel(&actual, 28, 52), [0, 0, 255, 255]);
    let translucent = pixel(&actual, 68, 12);
    assert_eq!(
        (translucent[0], translucent[2], translucent[3]),
        (0, 0, 255)
    );
    assert!((187..=189).contains(&translucent[1]));
    assert_eq!(pixel(&actual, 68, 52), [0, 0, 0, 255]);
    assert!(matches!(
        loader.load_with_fs("/asset.data", fs.as_ref()),
        Err(ImageLoadError::Decode(ImageDecodeError::UnsupportedInput)),
    ));
    assert_eq!(prepared.mobject().pixels(), PIXELS);
}

fn gamma_png() -> Vec<u8> {
    let source = encode_rgba8(1, 1, &[128, 128, 128, 255], CompressionLevel::Fast);
    let mut result = source[..8].to_vec();
    let mut at = 8;
    while at < source.len() {
        let len = u32::from_be_bytes(source[at..at + 4].try_into().unwrap()) as usize;
        let kind = &source[at + 4..at + 8];
        if kind == b"gAMA" {
            let mut body = b"gAMA".to_vec();
            body.extend_from_slice(&100_000u32.to_be_bytes());
            result.extend_from_slice(&4u32.to_be_bytes());
            result.extend_from_slice(&body);
            result.extend_from_slice(&fmn_codec::checksum::crc32(&body).to_be_bytes());
        } else if kind != b"sRGB" {
            result.extend_from_slice(&source[at..at + len + 12]);
        }
        at += len + 12;
    }
    result
}

#[test]
fn gamma_intent_survives_preparation_and_changes_actual_linear_light_rendering() {
    let mut loader = ImageLoader::new(ImageLoadOptions::default()).unwrap();
    let prepared = loader.decode(&gamma_png()).unwrap();
    assert_eq!(
        prepared.report().color_intent(),
        ImageColorIntent::Gamma {
            gamma_100000: 100_000
        },
    );
    let mobject: Mobject = prepared.mobject().into();
    assert_eq!(
        mobject.image.unwrap().color_space(),
        ImageColorSpace::Gamma(100_000)
    );
    let fs = Arc::new(VirtualFs::new());
    let actual = render_image(fs.clone(), prepared.mobject(), "/gamma.png");
    let srgb = render_image(
        fs,
        ImageMobject::from_rgba8(1, 1, vec![128, 128, 128, 255]).unwrap(),
        "/srgb.png",
    );
    let linear_sample = pixel(&actual, 48, 32);
    assert!((187..=189).contains(&linear_sample[0]));
    assert_eq!(linear_sample[0], linear_sample[1]);
    assert_eq!(linear_sample[1], linear_sample[2]);
    assert_eq!(pixel(&srgb, 48, 32), [128, 128, 128, 255]);
    assert_ne!(actual, srgb);
}

struct CountingLocator(Arc<AtomicUsize>);

impl FfmpegLocator for CountingLocator {
    fn locate_ffmpeg(&self, _: &Path) -> Result<FfmpegExecutable, FfmpegLocatorError> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Err(FfmpegLocatorError::NotFound)
    }
}

fn probing_options() -> (ImageLoadOptions, Arc<AtomicUsize>) {
    let calls = Arc::new(AtomicUsize::new(0));
    let options = ImageLoadOptions {
        ffmpeg: Some(FfmpegCapability {
            runner: Arc::new(NoProcessRunner),
            locator: Arc::new(CountingLocator(calls.clone())),
            workdir_root: PathBuf::from("/unused-image-workdir"),
        }),
        ..ImageLoadOptions::default()
    };
    (options, calls)
}

#[test]
fn native_jpeg_png_and_unknown_or_damaged_bytes_never_discover_ffmpeg() {
    let (options, calls) = probing_options();
    let mut loader = ImageLoader::new(options).unwrap();
    loader.decode(&png()).unwrap();
    let jpeg = include_bytes!("../../fmn-codec/tests/fixtures/jpeg/orient6_le.jpg");
    let prepared = loader.decode(jpeg).unwrap();
    let expected = ImageMobject::from_jpeg(jpeg).unwrap();
    assert_eq!(prepared.mobject(), expected);
    assert_eq!(prepared.report().decode().format, ImageInputFormat::Jpeg);
    assert!(prepared.report().decode().invocation.is_none());
    assert_eq!(
        prepared.report().color_intent(),
        ImageColorIntent::AssumedSrgb
    );
    assert!(matches!(
        loader.decode(b"\x89PNG\r\n\x1a\n"),
        Err(ImageLoadError::Decode(ImageDecodeError::NativePng(_))),
    ));
    assert!(matches!(
        loader.decode(b"\xff\xd8\xff"),
        Err(ImageLoadError::Decode(ImageDecodeError::NativeJpeg(_))),
    ));
    for bytes in [
        b"https://example.invalid/image.gif".as_slice(),
        b"<svg/>",
        b"#EXTM3U\n",
    ] {
        assert!(matches!(
            loader.decode(bytes),
            Err(ImageLoadError::Decode(ImageDecodeError::UnsupportedInput)),
        ));
    }
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    assert!(matches!(
        loader.decode(b"GIF89a\x02\x00\x02\x00"),
        Err(ImageLoadError::FfmpegUnavailable(_)),
    ));
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[test]
fn native_only_and_certified_policy_refuse_exotic_inputs_before_any_tool_lookup() {
    let (mut options, calls) = probing_options();
    options.determinism = DeterminismMode::Certified;
    let mut certified = ImageLoader::new(options).unwrap();
    let mut native = ImageLoader::new(ImageLoadOptions::default()).unwrap();
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
        assert!(matches!(
            certified.decode(bytes),
            Err(ImageLoadError::CertifiedTranscode { format: actual }) if actual == format,
        ));
        let error = native.decode(bytes).unwrap_err();
        assert!(error.to_string().contains("PNG or JPEG"));
        assert!(matches!(
            error,
            ImageLoadError::Decode(ImageDecodeError::TranscoderRequired { format: actual })
                if actual == format,
        ));
    }
    certified.decode(&png()).unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}

/// A source capability that permits only one bounded read. Any metadata or
/// unbounded read is a test failure; no path is ever handed to the decoder.
struct ReadOnceFs {
    bytes: Vec<u8>,
    reads: AtomicUsize,
}

impl FileSystem for ReadOnceFs {
    fn read_bounded(&self, path: &Path, max_bytes: usize) -> Result<Vec<u8>, FsError> {
        assert_eq!(self.reads.fetch_add(1, Ordering::SeqCst), 0);
        if self.bytes.len() > max_bytes {
            Err(FsError::TooLarge {
                path: path.into(),
                limit: max_bytes,
            })
        } else {
            Ok(self.bytes.clone())
        }
    }
    fn read(&self, _: &Path) -> Result<Vec<u8>, FsError> {
        panic!("unbounded read")
    }
    fn node_kind_no_follow(&self, _: &Path) -> Result<Option<FsNodeKind>, FsError> {
        panic!("metadata read")
    }
    fn create_dir(&self, _: &Path) -> Result<bool, FsError> {
        panic!("directory write")
    }
    fn write_atomic(&self, _: &Path, _: &[u8]) -> Result<(), FsError> {
        panic!("write")
    }
    fn create_new(&self, _: &Path, _: &[u8]) -> Result<bool, FsError> {
        panic!("write")
    }
    fn remove_file(&self, _: &Path) -> Result<(), FsError> {
        panic!("remove")
    }
    fn remove_dir_all(&self, _: &Path) -> Result<(), FsError> {
        panic!("remove")
    }
    fn exists(&self, _: &Path) -> bool {
        panic!("existence read")
    }
    fn list_dir(&self, _: &Path) -> Result<Vec<PathBuf>, FsError> {
        panic!("directory read")
    }
    fn count_dir_entries_bounded(&self, _: &Path, _: usize) -> Result<usize, FsError> {
        panic!("directory read")
    }
}

#[test]
fn bounded_file_read_is_inclusive_single_and_precedes_decode_or_discovery() {
    for (extra, success) in [(0, true), (1, false)] {
        let bytes = png();
        let (mut options, calls) = probing_options();
        options.limits.max_input_bytes = bytes.len() as u64 - extra;
        let fs = ReadOnceFs {
            bytes,
            reads: AtomicUsize::new(0),
        };
        let result = ImageLoader::new(options)
            .unwrap()
            .load_with_fs("/input", &fs);
        if success {
            assert_eq!(result.unwrap().mobject().pixels(), PIXELS);
        } else {
            assert!(matches!(
                result,
                Err(ImageLoadError::Read(FsError::TooLarge { .. }))
            ));
        }
        assert_eq!(fs.reads.load(Ordering::SeqCst), 1);
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }
    let fs = VirtualFs::new();
    assert!(matches!(
        ImageLoader::new(ImageLoadOptions::default())
            .unwrap()
            .load_with_fs("/missing", &fs),
        Err(ImageLoadError::Read(FsError::NotFound { .. })),
    ));
}

#[test]
fn preparation_budgets_fail_before_external_capability_use() {
    let (mut options, calls) = probing_options();
    options.ffmpeg_limits.timeout = std::time::Duration::ZERO;
    assert!(matches!(
        ImageLoader::new(options),
        Err(ImageLoadError::Decode(ImageDecodeError::InvalidJobLimits(
            _
        ))),
    ));
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    let mut options = ImageLoadOptions::default();
    options.limits.max_pixels = 3;
    assert!(matches!(
        ImageLoader::new(options).unwrap().decode(&png()),
        Err(ImageLoadError::Decode(ImageDecodeError::NativePng(_))),
    ));
    let mut options = ImageLoadOptions::default();
    options.limits.max_input_bytes = 8;
    assert!(matches!(
        ImageLoader::new(options).unwrap().decode(&png()),
        Err(ImageLoadError::Decode(
            ImageDecodeError::InputOversized { .. }
        )),
    ));
    let mut options = ImageLoadOptions::default();
    options.limits.max_chunks = 0;
    assert!(matches!(
        ImageLoader::new(options),
        Err(ImageLoadError::Decode(ImageDecodeError::InvalidLimits)),
    ));
}

#[test]
fn governed_raster_preparation_renders_first_frames_and_keeps_exact_decoder_receipts() {
    let executable: PathBuf = match std::env::var_os("FMN_IMAGE_FFMPEG") {
        Some(path) => path.into(),
        None if std::env::var_os("FMN_REQUIRE_FFMPEG").is_some() => "ffmpeg".into(),
        None => {
            eprintln!("native image external decode: set FMN_IMAGE_FFMPEG or FMN_REQUIRE_FFMPEG");
            return;
        }
    };
    struct ObservedLocator {
        inner: StdFfmpegLocator,
        calls: Arc<AtomicUsize>,
    }
    impl FfmpegLocator for ObservedLocator {
        fn locate_ffmpeg(&self, path: &Path) -> Result<FfmpegExecutable, FfmpegLocatorError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            self.inner.locate_ffmpeg(path)
        }
    }
    // Independently encoded 2x2 fixtures. The GIF has a second, black frame
    // which must not become the prepared image or an extra scene capture.
    const FIXTURES: [(ImageInputFormat, &str); 4] = [
        (
            ImageInputFormat::Gif,
            "474946383961020002008100000000ff00ff00ff0000ffff0021ff0b4e45545343415045322e30030100000021f904000a0000002c0000000002000200000807000504003020200021f904010a0001002c0000000002000200810000000000000000000000000806000108041010003b",
        ),
        (
            ImageInputFormat::Bmp,
            "424d460000000000000036000000280000000200000002000000010020000000000010000000c40e0000c40e00000000000000000000ff0000ff00ffffff0000ffff00ff00ff",
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
    let fs = Arc::new(VirtualFs::new());
    let calls = Arc::new(AtomicUsize::new(0));
    let mut loader = ImageLoader::new(ImageLoadOptions {
        ffmpeg: Some(FfmpegCapability {
            runner: Arc::new(StdProcessRunner),
            locator: Arc::new(ObservedLocator {
                inner: StdFfmpegLocator::from_host_path(),
                calls: calls.clone(),
            }),
            workdir_root: std::env::temp_dir(),
        }),
        ffmpeg_bin: executable,
        ..ImageLoadOptions::default()
    })
    .unwrap();
    let expected_pixels = [
        255, 0, 0, 255, 0, 255, 0, 255, 0, 0, 255, 255, 255, 255, 0, 255,
    ];
    let expected = render_image_with_mode(
        fs.clone(),
        ImageMobject::from_rgba8(2, 2, expected_pixels.to_vec()).unwrap(),
        "/exotic-expected.png",
        DeterminismMode::Standard,
    );
    for (format, hex) in FIXTURES {
        let bytes: Vec<u8> = hex
            .as_bytes()
            .as_chunks::<2>()
            .0
            .iter()
            .map(|pair| u8::from_str_radix(std::str::from_utf8(pair).unwrap(), 16).unwrap())
            .collect();
        let path = format!("/image.{}", format.name());
        fs.insert(&path, bytes.clone());
        let prepared = loader.load_with_fs(&path, fs.as_ref()).unwrap();
        assert_eq!(prepared.mobject().pixels(), expected_pixels);
        let report = prepared.report();
        assert_eq!(report.source_bytes(), bytes.len() as u64);
        assert_eq!(report.source_path(), Some(Path::new(&path)));
        assert_eq!(report.decode().format, format);
        if format == ImageInputFormat::Gif {
            assert_eq!(
                report.decode().source_digest.to_hex(),
                "b1cd5a643623e18baac5ef0e593a7ed3ee4286d77328ad02929cfba33a5163a5",
            );
        }
        let invocation = report.decode().invocation.as_ref().unwrap();
        assert_eq!(invocation.provenance.encoder.as_deref(), Some("png"));
        assert_eq!(invocation.provenance.tool_sha256_hex.len(), 64);
        assert!(
            invocation
                .provenance
                .argv
                .windows(2)
                .any(|p| p == ["-f", format.demuxer()])
        );
        assert!(
            invocation
                .provenance
                .argv
                .windows(2)
                .any(|p| p == ["-frames:v", "1"])
        );
        assert!(!invocation.artifact.exists());
        fs.insert(&path, b"changed input".to_vec());
        let actual = render_image_with_mode(
            fs.clone(),
            prepared.mobject(),
            &format!("/{}.png", format.name()),
            DeterminismMode::Standard,
        );
        assert_eq!(
            actual,
            expected,
            "{} changed rendered pixels",
            format.name()
        );
    }
    assert_eq!(
        calls.load(Ordering::SeqCst),
        1,
        "the resolved boundary is reused"
    );
    // One loader continues to admit native images without changing provenance.
    assert!(
        loader
            .decode(&png())
            .unwrap()
            .report()
            .decode()
            .invocation
            .is_none()
    );
}
