//! Native-first raster decoding over the one governed media boundary.
//!
//! PNG and JPEG always use the owned decoders. Explicitly recognized GIF,
//! BMP, WebP and TIFF inputs may use an injected or opt-in ffmpeg capability.
//! An image is one raster: animated inputs contribute their first frame.
//! External decoding is reported, never represented as certified native work.

use std::path::PathBuf;
use std::sync::Arc;

use fmn_codec::png::ColorIntent;
use fmn_codec::{JpegError, JpegLimits, PngError, PngLimits, decode_jpeg, decode_png};
use fmn_hash::{Digest, Sha256, sha256};
#[cfg(not(all(not(target_arch = "wasm32"), feature = "exact-process")))]
use fmn_platform::process::NoProcessRunner as HostProcessRunner;
#[cfg(all(not(target_arch = "wasm32"), feature = "exact-process"))]
use fmn_platform::process::StdProcessRunner as HostProcessRunner;
use fmn_platform::process::{FfmpegLocator, StdFfmpegLocator};

use crate::{Boundary, BoundaryError, FfmpegTool, InvocationReport, JobLimits};

/// Byte-recognized raster containers. No filename, URL or playlist selects a
/// demuxer, and native formats never fall through after a decoder refusal.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ImageInputFormat {
    Png,
    Jpeg,
    Gif,
    Bmp,
    WebP,
    Tiff,
}

impl ImageInputFormat {
    /// Stable format name for diagnostics and input receipts.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Png => "png",
            Self::Jpeg => "jpeg",
            Self::Gif => "gif",
            Self::Bmp => "bmp",
            Self::WebP => "webp",
            Self::Tiff => "tiff",
        }
    }

    /// The exact ffmpeg demuxer; image sequences and format probing are disabled.
    #[must_use]
    pub const fn demuxer(self) -> &'static str {
        match self {
            Self::Png => "png_pipe",
            Self::Jpeg => "jpeg_pipe",
            Self::Gif => "gif",
            Self::Bmp => "bmp_pipe",
            Self::WebP => "webp_pipe",
            Self::Tiff => "tiff_pipe",
        }
    }

    pub(crate) fn recognize(bytes: &[u8]) -> Option<Self> {
        if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
            Some(Self::Png)
        } else if bytes.starts_with(b"\xff\xd8\xff") {
            Some(Self::Jpeg)
        } else if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
            Some(Self::Gif)
        } else if bytes.starts_with(b"BM") {
            Some(Self::Bmp)
        } else if bytes.len() >= 12 && &bytes[..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
            Some(Self::WebP)
        } else if bytes.starts_with(b"II\x2a\x00") || bytes.starts_with(b"MM\x00\x2a") {
            Some(Self::Tiff)
        } else {
            None
        }
    }
}

/// Independent encoded-input, decoded-pixel and PNG-framing budgets.
#[derive(Clone, Debug)]
pub struct ImageDecodeLimits {
    pub max_input_bytes: u64,
    pub max_pixels: u64,
    pub max_chunks: usize,
}

impl Default for ImageDecodeLimits {
    fn default() -> Self {
        let png = PngLimits::default();
        Self {
            max_input_bytes: 64 * 1024 * 1024,
            max_pixels: png.max_pixels,
            max_chunks: png.max_chunks,
        }
    }
}

impl ImageDecodeLimits {
    pub(crate) fn decoded_byte_limit(&self) -> Result<u64, ImageDecodeError> {
        if self.max_input_bytes == 0
            || usize::try_from(self.max_input_bytes).is_err()
            || self.max_pixels == 0
            || self.max_pixels > i32::MAX as u64
            || self.max_chunks == 0
        {
            return Err(ImageDecodeError::InvalidLimits);
        }
        // RGBA plus one filter byte per row, DEFLATE framing and bounded PNG
        // metadata. Even a one-pixel-wide raster fits without geometric scaling.
        self.max_pixels
            .checked_mul(6)
            .and_then(|n| n.checked_add(65_536))
            .filter(|n| usize::try_from(*n).is_ok())
            .ok_or(ImageDecodeError::InvalidLimits)
    }

    pub(crate) fn png_limits(&self) -> PngLimits {
        PngLimits {
            max_pixels: self.max_pixels,
            max_chunks: self.max_chunks,
        }
    }
}

#[derive(Debug)]
pub enum ImageDecodeError {
    InvalidLimits,
    InvalidJobLimits(&'static str),
    InputOversized { bytes: u64, max: u64 },
    UnsupportedInput,
    TranscoderRequired { format: ImageInputFormat },
    Capability(String),
    NativePng(PngError),
    NativeJpeg(JpegError),
    InvalidRaster(&'static str),
    Boundary(BoundaryError),
}

impl std::fmt::Display for ImageDecodeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidLimits => f.write_str("image decode limits must be nonzero and fit native pixel/address-space bounds"),
            Self::InvalidJobLimits(detail) => write!(f, "image ffmpeg policy: {detail}"),
            Self::InputOversized { bytes, max } => write!(f, "image input of {bytes} bytes exceeds the {max}-byte budget"),
            Self::UnsupportedInput => f.write_str("unrecognized raster image; use PNG/JPEG natively, or GIF/BMP/WebP/TIFF with the optional governed ffmpeg decoder; URLs and playlists are not image assets"),
            Self::TranscoderRequired { format } => write!(f, "{} image requires the optional governed ffmpeg decoder; supply PNG or JPEG for native-only or certified rendering", format.name()),
            Self::Capability(detail) => write!(f, "image decode capability unavailable: {detail}; supply PNG or JPEG to avoid ffmpeg"),
            Self::NativePng(error) => write!(f, "native PNG decode: {error}"),
            Self::NativeJpeg(error) => write!(f, "native JPEG decode: {error}"),
            Self::InvalidRaster(detail) => write!(f, "decoded image: {detail}"),
            Self::Boundary(error) => write!(f, "image media boundary: {error}"),
        }
    }
}

impl std::error::Error for ImageDecodeError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::NativePng(error) => Some(error),
            Self::NativeJpeg(error) => Some(error),
            Self::Boundary(error) => Some(error),
            _ => None,
        }
    }
}

impl From<BoundaryError> for ImageDecodeError {
    fn from(error: BoundaryError) -> Self {
        Self::Boundary(error)
    }
}

/// Immutable preparation value in native output orientation (top row first).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RasterImage {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
    pub intent: ColorIntent,
}

impl From<fmn_codec::png::DecodedPng> for RasterImage {
    fn from(image: fmn_codec::png::DecodedPng) -> Self {
        Self {
            width: image.width,
            height: image.height,
            rgba: image.rgba,
            intent: image.intent,
        }
    }
}

/// Exact input and decoded-raster identities, including every external decode.
#[derive(Clone, Debug)]
pub struct ImageDecodeReport {
    pub format: ImageInputFormat,
    pub source_digest: Digest,
    /// Versioned normalized dimensions, transfer intent and RGBA8 bytes.
    pub pixel_digest: Digest,
    pub invocation: Option<InvocationReport>,
}

#[derive(Clone, Debug)]
pub struct DecodedImage {
    pub image: RasterImage,
    pub report: ImageDecodeReport,
}

pub(crate) fn decoded_image(
    source: &[u8],
    image: RasterImage,
    format: ImageInputFormat,
    invocation: Option<InvocationReport>,
) -> Result<DecodedImage, ImageDecodeError> {
    let expected_bytes = u64::from(image.width)
        .checked_mul(u64::from(image.height))
        .and_then(|pixels| pixels.checked_mul(4));
    if image.width == 0 || image.height == 0 || expected_bytes != Some(image.rgba.len() as u64) {
        return Err(ImageDecodeError::InvalidRaster(
            "invalid RGBA8 dimensions or byte count",
        ));
    }
    let mut hash = Sha256::new();
    hash.update(b"fmn-normalized-raster-v1\0");
    hash.update(&image.width.to_le_bytes());
    hash.update(&image.height.to_le_bytes());
    match image.intent {
        ColorIntent::Gamma { gamma_100000 } => {
            hash.update(&[1]);
            hash.update(&gamma_100000.to_le_bytes());
        }
        ColorIntent::Srgb { .. } | ColorIntent::AssumedSrgb => hash.update(&[0]),
    }
    hash.update(&image.rgba);
    Ok(DecodedImage {
        report: ImageDecodeReport {
            format,
            source_digest: sha256(source),
            pixel_digest: hash.finalize(),
            invocation,
        },
        image,
    })
}

/// Lazy, reusable decoder. `None` means native-only; PNG/JPEG never inspect
/// PATH, resolve an executable or invoke a process, even when one is configured.
pub struct ImageDecoder {
    limits: ImageDecodeLimits,
    job_limits: JobLimits,
    ffmpeg_bin: Option<PathBuf>,
    boundary: Option<Boundary>,
    locator: StdFfmpegLocator,
    workdir_root: Option<PathBuf>,
}

impl ImageDecoder {
    pub fn new(
        limits: ImageDecodeLimits,
        ffmpeg_bin: Option<PathBuf>,
    ) -> Result<Self, ImageDecodeError> {
        let max_artifact_bytes = limits.decoded_byte_limit()?;
        if ffmpeg_bin
            .as_ref()
            .is_some_and(|p| p.as_os_str().is_empty())
        {
            return Err(ImageDecodeError::Capability("empty ffmpeg path".into()));
        }
        if cfg!(target_arch = "wasm32") && ffmpeg_bin.is_some() {
            return Err(ImageDecodeError::Capability(
                "host image transcoding is unavailable on wasm32".into(),
            ));
        }
        let ffmpeg_bin = ffmpeg_bin
            .map(|path| {
                if path.is_relative() && path.components().count() > 1 {
                    std::env::current_dir()
                        .map(|cwd| cwd.join(path))
                        .map_err(|e| ImageDecodeError::Capability(e.to_string()))
                } else {
                    Ok(path)
                }
            })
            .transpose()?;
        let (locator, workdir_root) = if ffmpeg_bin.is_some() {
            (
                StdFfmpegLocator::from_host_path(),
                Some(std::env::temp_dir()),
            )
        } else {
            (StdFfmpegLocator::default(), None)
        };
        Ok(Self {
            limits,
            job_limits: JobLimits {
                max_artifact_bytes,
                ..JobLimits::default()
            },
            ffmpeg_bin,
            boundary: None,
            locator,
            workdir_root,
        })
    }

    /// Set process limits before the lazy boundary is resolved. Native decoding
    /// retains its independent budgets and never depends on this capability.
    pub fn with_job_limits(mut self, limits: JobLimits) -> Result<Self, ImageDecodeError> {
        if self.boundary.is_some() {
            return Err(ImageDecodeError::InvalidJobLimits(
                "cannot replace limits after the decoder boundary is resolved",
            ));
        }
        self.job_limits = crate::admit_render_ffmpeg_limits(&limits, limits.max_artifact_bytes)
            .map_err(ImageDecodeError::InvalidJobLimits)?;
        self.job_limits.max_artifact_bytes = self
            .job_limits
            .max_artifact_bytes
            .min(self.limits.decoded_byte_limit()?);
        Ok(self)
    }

    /// Inject an existing governed boundary without enabling a host runner.
    pub fn with_boundary(
        limits: ImageDecodeLimits,
        boundary: Boundary,
    ) -> Result<Self, ImageDecodeError> {
        limits.decoded_byte_limit()?;
        Ok(Self {
            limits,
            job_limits: JobLimits::default(),
            ffmpeg_bin: None,
            boundary: Some(boundary),
            locator: StdFfmpegLocator::default(),
            workdir_root: None,
        })
    }

    pub fn decode(&mut self, bytes: &[u8]) -> Result<DecodedImage, ImageDecodeError> {
        if bytes.len() as u64 > self.limits.max_input_bytes {
            return Err(ImageDecodeError::InputOversized {
                bytes: bytes.len() as u64,
                max: self.limits.max_input_bytes,
            });
        }
        let format =
            ImageInputFormat::recognize(bytes).ok_or(ImageDecodeError::UnsupportedInput)?;
        match format {
            ImageInputFormat::Png => {
                let image = decode_png(bytes, &self.limits.png_limits())
                    .map_err(ImageDecodeError::NativePng)?;
                return decoded_image(bytes, image.into(), format, None);
            }
            ImageInputFormat::Jpeg => {
                let image = decode_jpeg(
                    bytes,
                    &JpegLimits {
                        max_pixels: self.limits.max_pixels,
                    },
                )
                .map_err(ImageDecodeError::NativeJpeg)?;
                return decoded_image(
                    bytes,
                    RasterImage {
                        width: image.width,
                        height: image.height,
                        rgba: image.rgba,
                        intent: ColorIntent::AssumedSrgb,
                    },
                    format,
                    None,
                );
            }
            _ => {}
        }
        if self.boundary.is_none() {
            let path = self
                .ffmpeg_bin
                .as_ref()
                .ok_or(ImageDecodeError::TranscoderRequired { format })?;
            if !cfg!(all(not(target_arch = "wasm32"), feature = "exact-process")) {
                return Err(ImageDecodeError::Capability("host transcoding requires a native exact-process build or an injected boundary".into()));
            }
            let runner = Arc::new(HostProcessRunner);
            let executable = self
                .locator
                .locate_ffmpeg(path)
                .map_err(|e| ImageDecodeError::Capability(e.to_string()))?;
            let root = self
                .workdir_root
                .clone()
                .ok_or(ImageDecodeError::TranscoderRequired { format })?;
            let tool = FfmpegTool::resolve(executable, runner.as_ref(), &root)?;
            self.boundary = Some(Boundary::new(tool, runner, self.job_limits.clone(), root)?);
        }
        self.boundary
            .as_ref()
            .ok_or(ImageDecodeError::TranscoderRequired { format })?
            .decode_image_bytes(bytes, format, &self.limits)
    }
}
