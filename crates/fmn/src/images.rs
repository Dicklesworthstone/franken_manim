//! Bounded raster preparation for native Rust scenes.
//!
//! Prepare assets before scene construction, retain their receipts, and add
//! [`PreparedImage::mobject`] to a [`crate::Stage`]. PNG and JPEG use the native
//! codecs. GIF, BMP, WebP and TIFF require an explicitly supplied
//! [`FfmpegCapability`]; enabling a Cargo feature alone never authorizes a
//! process here. Animated inputs select their first frame.
//!
//! A receipt identifies the exact input bytes and immutable decoded pixels.
//! It is not a complete scene input-closure manifest, and is not automatically
//! attached to [`crate::RenderArtifact`]. Set [`ImageLoadOptions::determinism`]
//! to match the intended render; certified preparation refuses external
//! decoding before tool discovery. Retain the receipt with the caller's other
//! input attestations.

use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use fmn_config::config::DeterminismMode;
use fmn_library::{ImageError, ImageMobject};
use fmn_mobject::{ImageColorSpace, ImageResource, ImageSampler};
use fmn_output::{Boundary, DecodedImage, FfmpegTool, ImageDecoder, JobLimits};
use fmn_platform::fs::{FileSystem, FsError, StdFs};
use fmn_platform::process::FfmpegLocatorError;

use crate::FfmpegCapability;

pub use fmn_codec::png::ColorIntent as ImageColorIntent;
pub use fmn_output::{ImageDecodeError, ImageDecodeLimits, ImageDecodeReport, ImageInputFormat};

/// Input and process policy, fixed for the lifetime of an [`ImageLoader`].
#[derive(Clone, Debug)]
pub struct ImageLoadOptions {
    /// Input bytes, decoded pixels and PNG chunk limits.
    pub limits: ImageDecodeLimits,
    /// Certified mode admits native PNG/JPEG only, even with a capability.
    pub determinism: DeterminismMode,
    /// Optional, explicit authority to decode GIF/BMP/WebP/TIFF via ffmpeg.
    /// PNG/JPEG and unrecognized inputs never consult this capability.
    pub ffmpeg: Option<FfmpegCapability>,
    /// Executable request passed to the capability's locator on first use.
    pub ffmpeg_bin: PathBuf,
    /// Bounds on each external decode; native decodes use `limits` alone.
    pub ffmpeg_limits: JobLimits,
}

impl Default for ImageLoadOptions {
    fn default() -> Self {
        Self {
            limits: ImageDecodeLimits::default(),
            determinism: DeterminismMode::Standard,
            ffmpeg: None,
            ffmpeg_bin: PathBuf::from("ffmpeg"),
            ffmpeg_limits: JobLimits::default(),
        }
    }
}

/// A preparation failure, before an image can enter a scene.
#[derive(Debug)]
pub enum ImageLoadError {
    /// A bounded filesystem read failed.
    Read(FsError),
    /// Decoder policy, format, resource limit, or governed process failure.
    Decode(ImageDecodeError),
    /// This format needs an external decoder, outside certified preparation.
    CertifiedTranscode {
        /// Recognized input format.
        format: ImageInputFormat,
    },
    /// The explicitly supplied locator could not admit the requested ffmpeg.
    FfmpegUnavailable(FfmpegLocatorError),
    /// Decoded pixels could not form an immutable image mobject.
    Image(ImageError),
}

impl fmt::Display for ImageLoadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Read(error) => write!(f, "image input: {error}"),
            Self::Decode(error) => write!(f, "image preparation: {error}"),
            Self::CertifiedTranscode { format } => write!(
                f,
                "{} requires ffmpeg decoding, which is outside certified image preparation; supply a native PNG or JPEG input",
                format.name(),
            ),
            Self::FfmpegUnavailable(error) => write!(
                f,
                "image ffmpeg capability: {error}; supply PNG or JPEG for native decoding",
            ),
            Self::Image(error) => write!(f, "prepared image resource: {error}"),
        }
    }
}

impl std::error::Error for ImageLoadError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Read(error) => Some(error),
            Self::Decode(error) => Some(error),
            Self::FfmpegUnavailable(error) => Some(error),
            Self::Image(error) => Some(error),
            Self::CertifiedTranscode { .. } => None,
        }
    }
}

/// Immutable evidence for one successful preparation, not scene certification.
#[derive(Clone, Debug)]
pub struct ImageLoadReport {
    source_path: Option<PathBuf>,
    source_bytes: u64,
    color_intent: ImageColorIntent,
    decode: ImageDecodeReport,
}

impl ImageLoadReport {
    /// Requested filesystem path, or `None` for a byte-slice input.
    /// The content digest, rather than this descriptive path, identifies input.
    #[must_use]
    pub fn source_path(&self) -> Option<&Path> {
        self.source_path.as_deref()
    }

    /// Number of exact source bytes consumed by the decoder.
    #[must_use]
    pub const fn source_bytes(&self) -> u64 {
        self.source_bytes
    }

    /// Decoded transfer metadata, preserved in the image resource's color space.
    #[must_use]
    pub const fn color_intent(&self) -> ImageColorIntent {
        self.color_intent
    }

    /// Source and normalized-raster digests, recognized format, and any exact
    /// ffmpeg invocation/tool provenance. Native decodes have no invocation.
    #[must_use]
    pub const fn decode(&self) -> &ImageDecodeReport {
        &self.decode
    }
}

/// A renderable image and its receipt, with immutable, shared pixel storage.
/// Later source-file changes cannot alter this prepared value or its copies.
#[derive(Clone, Debug)]
pub struct PreparedImage {
    image: ImageMobject,
    report: ImageLoadReport,
}

impl PreparedImage {
    /// An independently styled image quad sharing the prepared pixel storage.
    /// Use the ordinary `with_height`, `with_opacity` and `with_z_index` builders
    /// before adding it to the stage. Image quads use the camera render route.
    #[must_use]
    pub fn mobject(&self) -> ImageMobject {
        self.image.clone()
    }

    /// Evidence for these exact pixels, unaffected by styling or scene edits.
    #[must_use]
    pub const fn report(&self) -> &ImageLoadReport {
        &self.report
    }
}

/// Reusable bounded loader. It reads each file once and lazily resolves at
/// most one governed ffmpeg boundary when an admitted format requires it.
pub struct ImageLoader {
    options: ImageLoadOptions,
    max_input_bytes: usize,
    decoder: ImageDecoder,
}

impl fmt::Debug for ImageLoader {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ImageLoader")
            .field("options", &self.options)
            .finish_non_exhaustive()
    }
}

impl ImageLoader {
    /// Validate policy without reading an asset or discovering a process.
    ///
    /// # Errors
    /// Refuses zero, overflowing, or otherwise invalid decode/process limits.
    pub fn new(options: ImageLoadOptions) -> Result<Self, ImageLoadError> {
        let decoder = ImageDecoder::new(options.limits.clone(), None)
            .and_then(|decoder| decoder.with_job_limits(options.ffmpeg_limits.clone()))
            .map_err(ImageLoadError::Decode)?;
        let max_input_bytes = usize::try_from(options.limits.max_input_bytes)
            .map_err(|_| ImageLoadError::Decode(ImageDecodeError::InvalidLimits))?;
        Ok(Self {
            options,
            max_input_bytes,
            decoder,
        })
    }

    /// Prepare bytes already owned by the host, with no filesystem read.
    ///
    /// # Errors
    /// Refuses invalid/oversized inputs, unavailable external capabilities,
    /// certified external decoding, and invalid raster resources.
    pub fn decode(&mut self, bytes: &[u8]) -> Result<PreparedImage, ImageLoadError> {
        self.prepare(bytes, None)
    }

    /// Read one bounded file through the host filesystem and prepare it.
    /// No extension guessing, directory search, URL fetching, or later reread.
    ///
    /// # Errors
    /// Filesystem read failures and the same refusals as [`Self::decode`].
    pub fn load(&mut self, path: impl AsRef<Path>) -> Result<PreparedImage, ImageLoadError> {
        self.load_with_fs(path, &StdFs)
    }

    /// Read exactly once through the supplied filesystem capability. The same
    /// bounded bytes are decoded and hashed, including external decoding.
    ///
    /// # Errors
    /// Filesystem read failures and the same refusals as [`Self::decode`].
    pub fn load_with_fs(
        &mut self,
        path: impl AsRef<Path>,
        fs: &dyn FileSystem,
    ) -> Result<PreparedImage, ImageLoadError> {
        let path = path.as_ref();
        let bytes = fs
            .read_bounded(path, self.max_input_bytes)
            .map_err(ImageLoadError::Read)?;
        self.prepare(&bytes, Some(path.to_path_buf()))
    }

    fn prepare(
        &mut self,
        bytes: &[u8],
        source_path: Option<PathBuf>,
    ) -> Result<PreparedImage, ImageLoadError> {
        let decoded = match self.decoder.decode(bytes) {
            Ok(decoded) => decoded,
            Err(ImageDecodeError::TranscoderRequired { format }) => {
                if self.options.determinism == DeterminismMode::Certified {
                    return Err(ImageLoadError::CertifiedTranscode { format });
                }
                self.resolve_decoder(format)?;
                self.decoder.decode(bytes).map_err(ImageLoadError::Decode)?
            }
            Err(error) => return Err(ImageLoadError::Decode(error)),
        };
        let DecodedImage { image, report } = decoded;
        let color_space = match image.intent {
            ImageColorIntent::Gamma { gamma_100000 } => ImageColorSpace::Gamma(gamma_100000),
            ImageColorIntent::Srgb { .. } | ImageColorIntent::AssumedSrgb => ImageColorSpace::Srgb,
        };
        let resource = ImageResource::rgba8(
            image.width,
            image.height,
            image.rgba,
            color_space,
            ImageSampler::default(),
        )
        .map_err(|error| ImageLoadError::Image(ImageError::Resource(error)))?;
        Ok(PreparedImage {
            image: ImageMobject::from_resource(resource).map_err(ImageLoadError::Image)?,
            report: ImageLoadReport {
                source_path,
                source_bytes: bytes.len() as u64,
                color_intent: image.intent,
                decode: report,
            },
        })
    }

    fn resolve_decoder(&mut self, format: ImageInputFormat) -> Result<(), ImageLoadError> {
        let capability = self.options.ffmpeg.as_ref().ok_or(ImageLoadError::Decode(
            ImageDecodeError::TranscoderRequired { format },
        ))?;
        let executable = capability
            .locator
            .locate_ffmpeg(&self.options.ffmpeg_bin)
            .map_err(ImageLoadError::FfmpegUnavailable)?;
        let boundary = FfmpegTool::resolve(
            executable,
            capability.runner.as_ref(),
            &capability.workdir_root,
        )
        .and_then(|tool| {
            Boundary::new(
                tool,
                Arc::clone(&capability.runner),
                self.options.ffmpeg_limits.clone(),
                capability.workdir_root.clone(),
            )
        })
        .map_err(|error| ImageLoadError::Decode(ImageDecodeError::Boundary(error)))?;
        self.decoder = ImageDecoder::with_boundary(self.options.limits.clone(), boundary)
            .map_err(ImageLoadError::Decode)?;
        Ok(())
    }
}
