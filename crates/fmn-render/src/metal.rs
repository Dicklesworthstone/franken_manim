//! The standard-only Metal annex executor.
//!
//! This module is the production descendant of G0-8 and G0-8b. It derives a
//! packed, single-typed device layouts from the same prepared [`FrameJob`] and
//! [`ThreeDJob`] the CPU engines consume, dispatches only through frankentorch's
//! safe generic gateway, and keeps its output surfaces alive across frames. The
//! semantic front-end, painter-ordered CSR command runs, fill-before-stroke
//! rule, camera clipping, and affine preparation therefore have one authority.
//!
//! The annex never participates in `certified`. Its public preview composition
//! root reports whether Metal actually produced a frame or whether the declared
//! fast-CPU fallback did; CPU bytes are never published under a Metal identity.

use std::fmt;
use std::time::{Duration, Instant};

use fmn_frame::{ChromaSiting, ColorRange, FrameBuffer, FrameError, FrameLayout, PixelFormat};
use fmn_hash::{Digest, Schema, Writer, sha256};
use fmn_mobject::JointType;
use ft_kernel_metal::Error as GatewayError;
use ft_kernel_metal::compute::{Gateway, Grid, MathMode, Pipeline, SharedBuffer};
#[cfg(feature = "synthetic-input")]
pub use ft_kernel_metal::presentation::SyntheticInput;
pub use ft_kernel_metal::presentation::{
    ButtonSet, INPUT_QUEUE_CAPACITY, InputEvent, LogicalPoint, Modifiers, PixelDensity,
    PointerButton, PresentOutcome, PresentationError, PresentationPipelineInfo, PresentationState,
    ScrollUnit, SurfaceMetrics,
};
use ft_kernel_metal::presentation::{NativePresenter, PresentationConfig};

use crate::bloom::{BLOOM_BAND_ROWS, BLOOM_SCRATCH_BYTES_PER_PIXEL, Bloom};
use crate::engine::{AaPolicy, Draw, EngineIdentity, FrameConfig, FrameJob, FrameJobError};
use crate::fill::MonoTable;
use crate::plan::RenderPlan;
use crate::texture::{Texture, TextureWrap};
use crate::three_d::{CompiledPrimitive, Shader, TextureMaterial, ThreeDJob};
use crate::{Binning, Segment};

const KERNEL_SOURCE: &str = include_str!("shaders/metal.metal");
const RASTER_KERNEL: &str = "fmn_render_frame";
const THREE_D_KERNEL: &str = "fmn_render_three_d";
const RGBA8_KERNEL: &str = "fmn_rgba16f_to_rgba8";
const NV12_KERNEL: &str = "fmn_rgba16f_to_nv12";
const P010_KERNEL: &str = "fmn_rgba16f_to_p010";
const BLOOM_ROWS_KERNEL: &str = "fmn_bloom_rows";
const BLOOM_COLUMNS_KERNEL: &str = "fmn_bloom_columns";
const BLOOM_KERNELS: &str = "fmn_bloom_rows and fmn_bloom_columns";
const SEGMENT_ARC_INTERVALS: usize = 16;
const SEGMENT_ARC_VALUES: usize = SEGMENT_ARC_INTERVALS + 1;
const SEGMENT_STRIDE: usize = 8 + SEGMENT_ARC_VALUES + 1;
const PIECE_STRIDE: usize = 6;
const JOIN_STRIDE: usize = 13;
const STATION_STRIDE: usize = 5;
const DRAW_U32_STRIDE: usize = 10;
const DRAW_F32_STRIDE: usize = 8;
const STYLE_STRIDE: usize = 20;
const THREE_D_VERTEX_STRIDE: usize = 17;
const THREE_D_TRIANGLE_STRIDE: usize = THREE_D_VERTEX_STRIDE * 3;
const THREE_D_DRAW_U32_STRIDE: usize = 5;
const THREE_D_DRAW_F32_STRIDE: usize = 24;
const THREE_D_TEXTURE_U32_STRIDE: usize = 8;
/// A draw row's texture row when it samples none, and a texture row's dark
/// texture when it has none.
const THREE_D_NO_TEXTURE: u32 = u32::MAX;
const STATUS_COMPLETE: u32 = 0x464d_4e4d;
const TRANSFER_TILE: usize = 16;
const TRANSFER_TABLE_ENTRIES: usize = 1 << 16;
const TRANSFER_TABLE_WORDS_PER_PLANE: usize = TRANSFER_TABLE_ENTRIES / 4;
const TRANSFER_TABLE_WORDS: usize = TRANSFER_TABLE_WORDS_PER_PLANE * 2;

const DRAW_FILL: u32 = 1 << 0;
const DRAW_STROKE: u32 = 1 << 1;
const STROKE_BEHIND: u32 = 1 << 2;
const FLAT_FILL: u32 = 1 << 3;
const MULTI_CONTOUR_GRADIENT: u32 = 1 << 4;
const THREE_D_SHADER_GOURAUD: u32 = 1;
const THREE_D_SHADER_DOT: u32 = 2;
const THREE_D_SHADER_TEXTURE: u32 = 3;

/// Canonical schema for the annex-specific half of C7.
pub const METAL_BACKEND_SCHEMA: Schema = Schema::new(*b"FMNM", 1, 0, 1);

/// Canonical schema for native Studio presentation provenance.
pub const NATIVE_PREVIEW_BACKEND_SCHEMA: Schema = Schema::new(*b"FMNP", 1, 0, 1);

/// Version-1 maximum linear-channel error for Metal versus certified.
///
/// The production three-frame corpus measures `0.1376953125` at the inner
/// border of one curved fill. This is the already-characterized `f32`
/// nearest-point conditioning residual from G0-8, not fill-area drift; the
/// blocking budget leaves roughly five percent headroom.
pub const METAL_VISUAL_BUDGET_V1_MAX_CHANNEL_ERROR: f64 = 0.145;

/// Version-1 RMS linear-channel error for Metal versus certified.
///
/// The production corpus maximum is `0.0020294770459577255`.
pub const METAL_VISUAL_BUDGET_V1_RMS_CHANNEL_ERROR: f64 = 0.002_14;

/// Version-1 minimum global sRGB-luma SSIM for Metal versus certified.
///
/// The production corpus minimum is `0.9998831140399521`; the bound applies
/// five percent headroom to perceptual distortion (`1 - SSIM`).
pub const METAL_VISUAL_BUDGET_V1_MIN_SSIM: f64 = 0.999_87;

/// Version-1 maximum linear-channel error for prepared 3D triangles.
///
/// This separately gates fixed-grid Gouraud surfaces and true/glow dots because
/// their perspective interpolation and radial falloff do not exercise the 2D
/// curve-distance residual measured above. The production corpus measures one
/// binary16 step (`0.00048828125`); the bound leaves five percent headroom.
pub const METAL_THREE_D_VISUAL_BUDGET_V1_MAX_CHANNEL_ERROR: f64 = 0.000_513;

/// Version-1 RMS linear-channel error for prepared 3D triangles.
///
/// The production corpus measures `0.0000013473872790351067`.
pub const METAL_THREE_D_VISUAL_BUDGET_V1_RMS_CHANNEL_ERROR: f64 = 0.000_001_42;

/// Version-1 minimum global sRGB-luma SSIM for prepared 3D triangles.
///
/// The production corpus measures `1.0`; the smoke-alarm threshold leaves one
/// part per million for cross-device standard-mode arithmetic.
pub const METAL_THREE_D_VISUAL_BUDGET_V1_MIN_SSIM: f64 = 0.999_999;

/// Maximum encoded-code error for the GPU RGBA16F-to-RGBA8 transfer.
///
/// Both implementations index the same authoritative Reel transfer tables by
/// raw binary16 bits, so this is exact rather than a visual-equivalence budget.
pub const METAL_RGBA8_TRANSFER_V1_MAX_CODE_ERROR: u8 = 0;

/// A Metal dispatch could not truthfully produce the requested frame.
#[derive(Debug)]
pub enum MetalError {
    /// The backend-neutral front-end rejected stale or unsupported inputs.
    FrameJob(FrameJobError),
    /// The requested frame layout was invalid.
    Frame(FrameError),
    /// The sanctioned frankentorch gateway refused or failed the operation.
    Gateway(GatewayError),
    /// A host-side count or byte calculation overflowed.
    SizeOverflow(&'static str),
    /// The selected fine tile exceeds the compiled pipeline's occupancy limit.
    ThreadgroupTooLarge {
        /// Pipeline entry point.
        kernel: &'static str,
        /// Requested threads.
        requested: usize,
        /// Introspected pipeline maximum.
        maximum: usize,
    },
    /// The gateway returned success but not every dispatched group completed.
    ///
    /// The pinned gateway's synchronous dispatch does not expose Metal command
    /// buffer status. Every kernel therefore writes a fresh per-group sentinel,
    /// and the wrapper fails closed when a runtime device error leaves any
    /// sentinel unwritten.
    IncompleteDispatch {
        /// Pipeline entry point.
        kernel: &'static str,
        /// Groups whose sentinel was present.
        completed: usize,
        /// Groups dispatched.
        expected: usize,
    },
    /// A backend-specific invariant drifted from its shader mirror.
    Layout(&'static str),
    /// Full record stroke profiles require the CPU renderer until the Metal
    /// annex carries an equivalent station table. Never flatten them silently.
    UnsupportedStrokeProfile,
    /// Full fill boundary colors currently require the analytic CPU field.
    UnsupportedFillProfile,
    /// The prepared 3D painter sequence contains a primitive not yet mapped by
    /// this annex pipeline.
    UnsupportedThreeDPrimitive {
        /// Painter-sequence index.
        draw: usize,
        /// Stable primitive description.
        primitive: &'static str,
    },
}

impl MetalError {
    fn unavailable(&self) -> bool {
        matches!(self, Self::Gateway(GatewayError::Unavailable))
    }

    fn permits_preview_fallback(&self) -> bool {
        matches!(
            self,
            Self::Gateway(_)
                | Self::ThreadgroupTooLarge { .. }
                | Self::IncompleteDispatch { .. }
                | Self::Layout(_)
        )
    }
}

impl fmt::Display for MetalError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::FrameJob(error) => error.fmt(f),
            Self::Frame(error) => error.fmt(f),
            Self::Gateway(error) => error.fmt(f),
            Self::SizeOverflow(what) => write!(f, "{what} exceeds this platform"),
            Self::ThreadgroupTooLarge {
                kernel,
                requested,
                maximum,
            } => write!(
                f,
                "{kernel} needs {requested} threads per threadgroup; the pipeline permits {maximum}"
            ),
            Self::IncompleteDispatch {
                kernel,
                completed,
                expected,
            } => write!(
                f,
                "{kernel} completed {completed} of {expected} threadgroups"
            ),
            Self::Layout(message) => write!(f, "Metal derived-layout mismatch: {message}"),
            Self::UnsupportedFillProfile => {
                f.write_str("Metal does not support retained fill profiles")
            }
            Self::UnsupportedStrokeProfile => {
                f.write_str("Metal does not support retained stroke profiles")
            }
            Self::UnsupportedThreeDPrimitive { draw, primitive } => write!(
                f,
                "Metal 3D draw {draw} uses unsupported prepared primitive {primitive}"
            ),
        }
    }
}

impl std::error::Error for MetalError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::FrameJob(error) => Some(error),
            Self::Frame(error) => Some(error),
            Self::Gateway(error) => Some(error),
            Self::SizeOverflow(_)
            | Self::ThreadgroupTooLarge { .. }
            | Self::IncompleteDispatch { .. }
            | Self::Layout(_)
            | Self::UnsupportedThreeDPrimitive { .. }
            | Self::UnsupportedStrokeProfile
            | Self::UnsupportedFillProfile => None,
        }
    }
}

impl From<FrameJobError> for MetalError {
    fn from(error: FrameJobError) -> Self {
        Self::FrameJob(error)
    }
}

impl From<FrameError> for MetalError {
    fn from(error: FrameError) -> Self {
        Self::Frame(error)
    }
}

impl From<GatewayError> for MetalError {
    fn from(error: GatewayError) -> Self {
        Self::Gateway(error)
    }
}

/// One production annex dispatch's PG-A and provenance record.
#[derive(Debug, Clone)]
pub struct MetalReport {
    /// Engine identity carried by the frame.
    pub identity: EngineIdentity,
    /// Metal device name.
    pub device: String,
    /// Whether the selected device reports unified memory.
    pub unified_memory: bool,
    /// Safe math is permanent for the first production annex.
    pub math_mode: &'static str,
    /// Hash of the exact embedded MSL source.
    pub kernel_digest: Digest,
    /// Hash of the exact binary16-to-byte tables resident on the device.
    pub transfer_table_digest: Digest,
    /// Fine-tile threads used by the raster pipeline.
    pub threads_per_threadgroup: usize,
    /// Raster pipeline occupancy ceiling.
    pub max_threads_per_threadgroup: usize,
    /// Raster pipeline SIMD execution width.
    pub thread_execution_width: usize,
    /// Bytes materialized into new input buffers for this frame.
    pub upload_bytes: usize,
    /// Frame-pixel bytes copied to the host for the requested output.
    ///
    /// Status sentinels are control-plane validation, not frame pixels. Native
    /// presentation reports zero here because its lifetime-held RGBA8 surface
    /// remains GPU-visible through the drawable handoff.
    pub readback_bytes: usize,
    /// Whether the renderer reused its lifetime-held raw surface.
    pub raw_surface_reused: bool,
    /// Whether a converted output surface was reused.
    pub output_surface_reused: bool,
    /// Host-observed prepare/upload/dispatch/output-handoff wall time.
    pub elapsed: Duration,
    /// Format copied back to the host.
    pub output_format: PixelFormat,
    /// Quantization range for a planar YUV output.
    pub color_range: Option<ColorRange>,
    /// Chroma siting for a planar YUV output.
    pub chroma_siting: Option<ChromaSiting>,
}

impl MetalReport {
    /// Stable annex backend identity for C7.
    ///
    /// Timing and per-frame byte counts are observations, not identity, and are
    /// deliberately excluded.
    #[must_use]
    pub fn backend_journal(&self) -> Vec<u8> {
        let mut writer = Writer::new(METAL_BACKEND_SCHEMA);
        writer.put_str(self.identity.engine.name());
        writer.put_u32(self.identity.renderer_version);
        writer.put_str(&self.device);
        writer.put_bool(self.unified_memory);
        writer.put_str(self.math_mode);
        writer.put_bytes(self.kernel_digest.as_bytes());
        writer.put_u64(self.threads_per_threadgroup as u64);
        writer.put_u64(self.max_threads_per_threadgroup as u64);
        writer.put_u64(self.thread_execution_width as u64);
        writer.put_str(format_name(self.output_format));
        writer.put_bytes(self.transfer_table_digest.as_bytes());
        writer.put_str(color_range_name(self.color_range));
        writer.put_str(chroma_siting_name(self.chroma_siting));
        writer
            .finish()
            .expect("the fixed-size Metal backend identity fits the schema limits")
    }

    /// Digest of [`MetalReport::backend_journal`].
    #[must_use]
    pub fn backend_digest(&self) -> Digest {
        sha256(&self.backend_journal())
    }

    /// Measured whole-call frame rate, when the timer had nonzero resolution.
    #[must_use]
    pub fn frames_per_second(&self) -> Option<f64> {
        let seconds = self.elapsed.as_secs_f64();
        (seconds > 0.0).then_some(1.0 / seconds)
    }
}

/// A native Studio frame could not render or reach its drawable.
#[derive(Debug)]
pub enum NativePreviewError {
    /// Lumen could not produce the lifetime-held RGBA8 surface.
    Render(MetalError),
    /// Frankentorch could not create, update, or present the native surface.
    Presentation(PresentationError),
}

impl NativePreviewError {
    /// Declared CPU-stream fallback, when this failure is backend-local.
    ///
    /// Caller input, frame-layout, and size failures remain errors. An
    /// unsupported 3D primitive is a frame-local fallback; other admitted
    /// backend and presentation failures demote the native route.
    #[must_use]
    pub fn stream_fallback(&self) -> Option<PreviewFallback> {
        match self {
            Self::Render(MetalError::Gateway(GatewayError::Unavailable))
            | Self::Presentation(PresentationError::Unavailable) => {
                Some(PreviewFallback::Unavailable)
            }
            Self::Render(
                error @ (MetalError::UnsupportedThreeDPrimitive { .. }
                | MetalError::UnsupportedStrokeProfile
                | MetalError::UnsupportedFillProfile),
            ) => Some(PreviewFallback::Unsupported(error.to_string())),
            Self::Render(error) if error.permits_preview_fallback() => {
                Some(PreviewFallback::BackendFailure(error.to_string()))
            }
            Self::Presentation(
                error @ (PresentationError::WrongThread
                | PresentationError::Window(_)
                | PresentationError::Pipeline(_)
                | PresentationError::Closed
                | PresentationError::CommandBuffer(_)),
            ) => Some(PreviewFallback::BackendFailure(error.to_string())),
            Self::Render(_)
            | Self::Presentation(
                PresentationError::InvalidDimensions { .. }
                | PresentationError::InvalidStride { .. }
                | PresentationError::SizeOverflow
                | PresentationError::BufferTooSmall { .. },
            ) => None,
        }
    }

    /// Declared fallback during native-route construction.
    ///
    /// A missing Metal device and presentation-surface failures may select the
    /// CPU stream. Renderer shader or pipeline construction failures remain
    /// product errors instead of being hidden by fallback.
    #[must_use]
    pub fn construction_stream_fallback(&self) -> Option<PreviewFallback> {
        match self {
            Self::Render(MetalError::Gateway(GatewayError::Unavailable))
            | Self::Presentation(_) => self.stream_fallback(),
            Self::Render(_) => None,
        }
    }

    /// Whether this is a per-frame capability gap rather than a dead backend.
    #[must_use]
    pub fn is_frame_local_unsupported(&self) -> bool {
        matches!(
            self,
            Self::Render(
                MetalError::UnsupportedThreeDPrimitive { .. }
                    | MetalError::UnsupportedStrokeProfile
                    | MetalError::UnsupportedFillProfile
            )
        )
    }
}

impl fmt::Display for NativePreviewError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Render(error) => error.fmt(f),
            Self::Presentation(error) => error.fmt(f),
        }
    }
}

impl std::error::Error for NativePreviewError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Render(error) => Some(error),
            Self::Presentation(error) => Some(error),
        }
    }
}

impl From<MetalError> for NativePreviewError {
    fn from(error: MetalError) -> Self {
        Self::Render(error)
    }
}

impl From<PresentationError> for NativePreviewError {
    fn from(error: PresentationError) -> Self {
        Self::Presentation(error)
    }
}

/// PG-A evidence for one native Studio presentation attempt.
#[derive(Debug, Clone)]
pub struct NativePreviewReport {
    /// Lumen render and transfer facts; `readback_bytes` is always zero.
    pub metal: MetalReport,
    /// Occupancy facts for frankentorch's drawable transfer pipeline.
    pub presentation: PresentationPipelineInfo,
    /// Whether a drawable was presented or temporarily unavailable.
    pub outcome: PresentOutcome,
}

impl NativePreviewReport {
    /// Stable combined renderer/presentation backend identity.
    #[must_use]
    pub fn backend_journal(&self) -> Vec<u8> {
        let mut writer = Writer::new(NATIVE_PREVIEW_BACKEND_SCHEMA);
        writer.put_bytes(&self.metal.backend_journal());
        writer.put_str("native-cametal-layer");
        writer.put_u64(self.presentation.threads_per_threadgroup[0] as u64);
        writer.put_u64(self.presentation.threads_per_threadgroup[1] as u64);
        writer.put_u64(self.presentation.max_threads_per_threadgroup as u64);
        writer.put_u64(self.presentation.thread_execution_width as u64);
        writer
            .finish()
            .expect("the fixed-size native preview identity fits the schema limits")
    }

    /// Digest of [`NativePreviewReport::backend_journal`].
    #[must_use]
    pub fn backend_digest(&self) -> Digest {
        sha256(&self.backend_journal())
    }

    /// Frame-pixel bytes copied back to host memory.
    #[must_use]
    pub fn frame_pixel_readback_bytes(&self) -> usize {
        self.metal.readback_bytes
    }
}

/// Why Studio preview used the CPU route.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PreviewFallback {
    /// This target or machine has no usable Metal device.
    Unavailable,
    /// The current prepared frame uses an annex feature that has not landed.
    Unsupported(String),
    /// A device existed but a later annex operation failed.
    BackendFailure(String),
}

/// Which engine actually produced a preview frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PreviewRoute {
    /// Metal produced the pixels.
    Metal,
    /// Fast CPU produced the pixels for the recorded reason.
    FastCpu(PreviewFallback),
}

/// One Studio-ready RGBA8 frame with truthful execution provenance.
#[derive(Debug)]
pub struct PreviewFrame {
    /// Tight, top-row-first RGBA8 pixels.
    pub frame: FrameBuffer,
    /// Actual execution route.
    pub route: PreviewRoute,
    /// PG-A record when Metal ran.
    pub metal: Option<MetalReport>,
}

impl PreviewFrame {
    /// The engine identity that truthfully produced these bytes.
    #[must_use]
    pub fn identity(&self) -> EngineIdentity {
        match self.route {
            PreviewRoute::Metal => EngineIdentity::metal(),
            PreviewRoute::FastCpu(_) => EngineIdentity::fast(),
        }
    }
}

/// Studio's renderer-owned selection and fallback boundary.
pub enum PreviewRenderer {
    /// Supported Metal device and compiled pipelines.
    Metal(Box<MetalRenderer>),
    /// Declared CPU fallback.
    FastCpu(PreviewFallback),
}

impl fmt::Debug for PreviewRenderer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Metal(renderer) => f.debug_tuple("Metal").field(renderer).finish(),
            Self::FastCpu(reason) => f.debug_tuple("FastCpu").field(reason).finish(),
        }
    }
}

impl PreviewRenderer {
    /// Prefer Metal and select the CPU fallback only when Metal is unavailable.
    ///
    /// A shader or pipeline error on a machine that claims Metal is a product
    /// defect and is returned, not mislabeled as hardware absence.
    pub fn new() -> Result<Self, MetalError> {
        match MetalRenderer::new() {
            Ok(renderer) => Ok(Self::Metal(Box::new(renderer))),
            Err(error) if error.unavailable() => Ok(Self::FastCpu(PreviewFallback::Unavailable)),
            Err(error) => Err(error),
        }
    }

    /// Render a Studio-ready RGBA8 frame.
    ///
    /// A runtime backend failure demotes this renderer to fast CPU and returns
    /// the CPU frame without turning a live preview dark. The returned route
    /// and identity make that fallback observable.
    pub fn render(
        &mut self,
        plan: &RenderPlan,
        mono: &MonoTable,
        binning: &Binning,
        config: FrameConfig,
        cpu_threads: usize,
    ) -> Result<PreviewFrame, MetalError> {
        let mut transient = false;
        let reason = match self {
            Self::Metal(renderer) => match renderer.render_rgba8(plan, mono, binning, config) {
                Ok((frame, report)) => {
                    return Ok(PreviewFrame {
                        frame,
                        route: PreviewRoute::Metal,
                        metal: Some(report),
                    });
                }
                Err(
                    error @ (MetalError::UnsupportedStrokeProfile
                    | MetalError::UnsupportedFillProfile),
                ) => {
                    transient = true;
                    PreviewFallback::Unsupported(error.to_string())
                }
                Err(error) if error.permits_preview_fallback() => {
                    PreviewFallback::BackendFailure(error.to_string())
                }
                Err(error) => return Err(error),
            },
            Self::FastCpu(reason) => reason.clone(),
        };
        if !transient {
            *self = Self::FastCpu(reason.clone());
        }
        let frame = render_cpu_rgba8(plan, mono, binning, config, cpu_threads)?;
        Ok(PreviewFrame {
            frame,
            route: PreviewRoute::FastCpu(reason),
            metal: None,
        })
    }

    /// Render a prepared 3D Studio frame through the same truthful annex/fallback
    /// boundary.
    ///
    /// An unsupported prepared primitive falls back for this frame only; it does
    /// not poison a healthy Metal device. A runtime backend failure retains the
    /// existing permanent demotion policy.
    pub fn render_three_d(
        &mut self,
        job: &ThreeDJob<'_>,
        cpu_threads: usize,
    ) -> Result<PreviewFrame, MetalError> {
        let mut transient = None;
        let reason = match self {
            Self::Metal(renderer) => match renderer.render_three_d_rgba8(job) {
                Ok((frame, report)) => {
                    return Ok(PreviewFrame {
                        frame,
                        route: PreviewRoute::Metal,
                        metal: Some(report),
                    });
                }
                Err(
                    error @ (MetalError::UnsupportedThreeDPrimitive { .. }
                    | MetalError::UnsupportedStrokeProfile
                    | MetalError::UnsupportedFillProfile),
                ) => {
                    let reason = PreviewFallback::Unsupported(error.to_string());
                    transient = Some(reason.clone());
                    reason
                }
                Err(error) if error.permits_preview_fallback() => {
                    PreviewFallback::BackendFailure(error.to_string())
                }
                Err(error) => return Err(error),
            },
            Self::FastCpu(reason) => reason.clone(),
        };
        if transient.is_none() {
            *self = Self::FastCpu(reason.clone());
        }
        let frame = render_cpu_three_d_rgba8(job, cpu_threads)?;
        Ok(PreviewFrame {
            frame,
            route: PreviewRoute::FastCpu(reason),
            metal: None,
        })
    }
}

/// Lifetime-held Metal pipelines and output surfaces.
pub struct MetalRenderer {
    gateway: Gateway,
    raster: Pipeline,
    three_d: Pipeline,
    rgba8: Pipeline,
    nv12: Pipeline,
    p010: Pipeline,
    bloom_rows: Pipeline,
    bloom_columns: Pipeline,
    transfer_table: SharedBuffer,
    raw_surface: Option<SurfaceSlot>,
    output_surface: Option<OutputSurfaceSlot>,
    /// The bloom's row sums, kept with the raw surface they belong to.
    bloom_sums: Option<SurfaceSlot>,
    /// The rows the bloom works in at once ([`BLOOM_BAND_ROWS`]; tests
    /// take others: the bits do not depend on it).
    bloom_band_rows: u32,
    /// The last 3D frame's texels, kept for the next frame that samples the
    /// same textures (FL-033).
    resident_texels: Option<ResidentTexels>,
    device: String,
    unified_memory: bool,
    kernel_digest: Digest,
    transfer_table_digest: Digest,
}

impl fmt::Debug for MetalRenderer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("MetalRenderer")
            .field("device", &self.device)
            .field("unified_memory", &self.unified_memory)
            .field(
                "raw_surface_bytes",
                &self.raw_surface.as_ref().map(|s| s.bytes),
            )
            .field(
                "output_surface_bytes",
                &self
                    .output_surface
                    .as_ref()
                    .map(|surface| surface.layout.total_bytes()),
            )
            .field("resident_texel_bytes", &self.resident_texel_bytes())
            .field("bloom_scratch_bytes", &self.bloom_scratch_bytes())
            .finish_non_exhaustive()
    }
}

impl MetalRenderer {
    /// Whether the current target exposes a Metal gateway.
    #[must_use]
    pub fn is_available() -> bool {
        Gateway::open().is_ok()
    }

    /// Open the device and compile the embedded safe-math pipelines once.
    pub fn new() -> Result<Self, MetalError> {
        let gateway = Gateway::open()?;
        let library = gateway.library_with(KERNEL_SOURCE, MathMode::Safe)?;
        let raster = library.pipeline(RASTER_KERNEL)?;
        let three_d = library.pipeline(THREE_D_KERNEL)?;
        let rgba8 = library.pipeline(RGBA8_KERNEL)?;
        let nv12 = library.pipeline(NV12_KERNEL)?;
        let p010 = library.pipeline(P010_KERNEL)?;
        let bloom_rows = library.pipeline(BLOOM_ROWS_KERNEL)?;
        let bloom_columns = library.pipeline(BLOOM_COLUMNS_KERNEL)?;
        let (transfer_words, transfer_table_digest) = build_transfer_table();
        let transfer_table = gateway.buffer_u32(&transfer_words)?;
        Ok(Self {
            gateway,
            raster,
            three_d,
            rgba8,
            nv12,
            p010,
            bloom_rows,
            bloom_columns,
            transfer_table,
            raw_surface: None,
            output_surface: None,
            bloom_sums: None,
            bloom_band_rows: BLOOM_BAND_ROWS,
            resident_texels: None,
            device: gateway.device_name(),
            unified_memory: gateway.has_unified_memory(),
            kernel_digest: sha256(KERNEL_SOURCE.as_bytes()),
            transfer_table_digest,
        })
    }

    /// The bytes of the 3D texels the renderer keeps between frames: the
    /// last 3D frame's textures, which the next frame sampling the same
    /// textures (equal content digests and sizes, in the same order) binds
    /// instead of uploading them again. Zero before a 3D frame.
    #[must_use]
    pub fn resident_texel_bytes(&self) -> usize {
        self.resident_texels
            .as_ref()
            .map_or(0, |resident| resident.bytes)
    }

    /// The bytes of the bloom's row sums the renderer keeps on the device
    /// ([`Bloom::scratch_bytes`] of the last bloomed 3D frame: a band and
    /// the blur's reach, not the whole frame), reused by the next bloomed
    /// frame of the same size and reach. Zero before one.
    #[must_use]
    pub fn bloom_scratch_bytes(&self) -> usize {
        self.bloom_sums.as_ref().map_or(0, |sums| sums.bytes)
    }

    /// Render the annex's linear-light Rgba16F comparison surface.
    pub fn render_raw(
        &mut self,
        plan: &RenderPlan,
        mono: &MonoTable,
        binning: &Binning,
        config: FrameConfig,
    ) -> Result<(FrameBuffer, MetalReport), MetalError> {
        let started = Instant::now();
        let job = FrameJob::for_metal(plan, mono, binning, config)?;
        let flat = FlatFrame::derive(&job)?;
        let raw_layout = config.layout()?;
        let (raw_reused, upload_bytes) = self.dispatch_raster(&flat, raw_layout.total_bytes())?;
        let mut frame = FrameBuffer::new(raw_layout);
        self.raw_surface
            .as_ref()
            .ok_or(MetalError::Layout("raw surface disappeared"))?
            .buffer
            .read_u8(frame.as_bytes_mut())?;
        let report = self.report(
            &self.raster,
            flat.tile().saturating_mul(flat.tile()),
            upload_bytes,
            frame.as_bytes().len(),
            raw_reused,
            false,
            started.elapsed(),
            PixelFormat::Rgba16F,
            None,
            None,
        );
        Ok((frame, report))
    }

    /// Render and transfer directly to Studio's tight RGBA8 payload.
    ///
    /// The linear half surface remains device-resident between raster and
    /// transfer; only the four-byte preview surface crosses to the host.
    pub fn render_rgba8(
        &mut self,
        plan: &RenderPlan,
        mono: &MonoTable,
        binning: &Binning,
        config: FrameConfig,
    ) -> Result<(FrameBuffer, MetalReport), MetalError> {
        let started = Instant::now();
        let dispatch = self.dispatch_rgba8_surface(plan, mono, binning, config)?;
        let frame = self.read_rgba8_surface()?;
        let report = self.report(
            &self.raster,
            dispatch.threads_per_threadgroup,
            dispatch.upload_bytes,
            frame.as_bytes().len(),
            dispatch.raw_surface_reused,
            dispatch.output_surface_reused,
            started.elapsed(),
            PixelFormat::Rgba8,
            None,
            None,
        );
        Ok((frame, report))
    }

    /// Render a prepared 3D painter sequence into its linear-light comparison
    /// surface.
    ///
    /// The annex accepts camera-clipped true/glow dots, Gouraud surfaces, and
    /// textured surfaces and images. All consume the exact triangle IR already
    /// prepared for [`ThreeDJob`]'s CPU executor; a frame's textures are uploaded
    /// once each as premultiplied linear `f32` texels and sampled as the CPU
    /// route samples them (bilinear, the material's wrap per axis, the dark
    /// texture's crossfade). The renderer keeps the last frame's texels on the
    /// device, and a frame sampling the same textures (equal content digests
    /// and sizes, in the same order) binds them instead of uploading them
    /// again ([`MetalRenderer::resident_texel_bytes`]). Retained vectors return
    /// a typed refusal so a preview owner can select its declared CPU fallback
    /// without mislabeling the bytes.
    pub fn render_three_d_raw(
        &mut self,
        job: &ThreeDJob<'_>,
    ) -> Result<(FrameBuffer, MetalReport), MetalError> {
        let started = Instant::now();
        let flat = FlatThreeDFrame::derive(job)?;
        let raw_layout = job.layout()?;
        let (raw_reused, upload_bytes) = self.dispatch_three_d(&flat, raw_layout.total_bytes())?;
        let mut frame = FrameBuffer::new(raw_layout);
        self.raw_surface
            .as_ref()
            .ok_or(MetalError::Layout("raw surface disappeared"))?
            .buffer
            .read_u8(frame.as_bytes_mut())?;
        let report = self.report(
            &self.three_d,
            flat.tile().saturating_mul(flat.tile()),
            upload_bytes,
            frame.as_bytes().len(),
            raw_reused,
            false,
            started.elapsed(),
            PixelFormat::Rgba16F,
            None,
            None,
        );
        Ok((frame, report))
    }

    /// Render a prepared dot/surface 3D frame and transfer it on-device to the
    /// tight RGBA8 Studio payload.
    pub fn render_three_d_rgba8(
        &mut self,
        job: &ThreeDJob<'_>,
    ) -> Result<(FrameBuffer, MetalReport), MetalError> {
        let started = Instant::now();
        let dispatch = self.dispatch_three_d_rgba8_surface(job)?;
        let frame = self.read_rgba8_surface()?;
        let report = self.report(
            &self.three_d,
            dispatch.threads_per_threadgroup,
            dispatch.upload_bytes,
            frame.as_bytes().len(),
            dispatch.raw_surface_reused,
            dispatch.output_surface_reused,
            started.elapsed(),
            PixelFormat::Rgba8,
            None,
            None,
        );
        Ok((frame, report))
    }

    fn dispatch_rgba8_surface(
        &mut self,
        plan: &RenderPlan,
        mono: &MonoTable,
        binning: &Binning,
        config: FrameConfig,
    ) -> Result<Rgba8SurfaceDispatch, MetalError> {
        let job = FrameJob::for_metal(plan, mono, binning, config)?;
        let flat = FlatFrame::derive(&job)?;
        let raw_layout = config.layout()?;
        let (raw_surface_reused, upload_bytes) =
            self.dispatch_raster(&flat, raw_layout.total_bytes())?;
        let (output_surface_reused, transfer_upload_bytes) =
            self.transfer_rgba8_surface(config.viewport.width, config.viewport.height)?;
        Ok(Rgba8SurfaceDispatch {
            width: config.viewport.width,
            height: config.viewport.height,
            threads_per_threadgroup: flat.tile().saturating_mul(flat.tile()),
            upload_bytes: upload_bytes
                .checked_add(transfer_upload_bytes)
                .ok_or(MetalError::SizeOverflow("frame upload count"))?,
            raw_surface_reused,
            output_surface_reused,
        })
    }

    fn dispatch_three_d_rgba8_surface(
        &mut self,
        job: &ThreeDJob<'_>,
    ) -> Result<Rgba8SurfaceDispatch, MetalError> {
        let flat = FlatThreeDFrame::derive(job)?;
        let layout = job.layout()?;
        let (raw_surface_reused, upload_bytes) =
            self.dispatch_three_d(&flat, layout.total_bytes())?;
        let (output_surface_reused, transfer_upload_bytes) =
            self.transfer_rgba8_surface(layout.width(), layout.height())?;
        Ok(Rgba8SurfaceDispatch {
            width: layout.width(),
            height: layout.height(),
            threads_per_threadgroup: flat.tile().saturating_mul(flat.tile()),
            upload_bytes: upload_bytes
                .checked_add(transfer_upload_bytes)
                .ok_or(MetalError::SizeOverflow("3D frame upload count"))?,
            raw_surface_reused,
            output_surface_reused,
        })
    }

    /// Render and convert on-device to a negotiated NV12 layout.
    ///
    /// The layout's per-plane strides are honored exactly. Only the compact
    /// Y′CbCr allocation crosses to the host; the linear RGBA16F surface stays
    /// device-resident.
    #[allow(clippy::too_many_arguments)]
    pub fn render_nv12(
        &mut self,
        plan: &RenderPlan,
        mono: &MonoTable,
        binning: &Binning,
        config: FrameConfig,
        output_layout: FrameLayout,
        range: ColorRange,
        siting: ChromaSiting,
    ) -> Result<(FrameBuffer, MetalReport), MetalError> {
        self.render_yuv420(
            plan,
            mono,
            binning,
            config,
            output_layout,
            PixelFormat::Nv12,
            range,
            siting,
        )
    }

    /// Render and convert on-device to a negotiated P010 layout.
    ///
    /// Reel defines P010 as limited-range only. Full range is therefore a
    /// typed refusal before any raster work is submitted.
    #[allow(clippy::too_many_arguments)]
    pub fn render_p010(
        &mut self,
        plan: &RenderPlan,
        mono: &MonoTable,
        binning: &Binning,
        config: FrameConfig,
        output_layout: FrameLayout,
        range: ColorRange,
        siting: ChromaSiting,
    ) -> Result<(FrameBuffer, MetalReport), MetalError> {
        self.render_yuv420(
            plan,
            mono,
            binning,
            config,
            output_layout,
            PixelFormat::P010,
            range,
            siting,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn render_yuv420(
        &mut self,
        plan: &RenderPlan,
        mono: &MonoTable,
        binning: &Binning,
        config: FrameConfig,
        output_layout: FrameLayout,
        expected_format: PixelFormat,
        range: ColorRange,
        siting: ChromaSiting,
    ) -> Result<(FrameBuffer, MetalReport), MetalError> {
        validate_yuv_output(&output_layout, config, expected_format, range)?;

        let started = Instant::now();
        let job = FrameJob::for_metal(plan, mono, binning, config)?;
        let flat = FlatFrame::derive(&job)?;
        let raw_layout = config.layout()?;
        let (raw_reused, upload_bytes) = self.dispatch_raster(&flat, raw_layout.total_bytes())?;
        let output_reused =
            ensure_output_surface(self.gateway, &mut self.output_surface, &output_layout)?;

        let sample_size = output_layout.format().sample_size();
        let transfer_params = [
            output_layout.width(),
            output_layout.height(),
            checked_u32(output_layout.stride(0) / sample_size, "YUV luma stride")?,
            checked_u32(output_layout.stride(1) / sample_size, "YUV chroma stride")?,
            checked_u32(
                output_layout.plane_offset(1) / sample_size,
                "YUV chroma offset",
            )?,
            color_range_code(range),
            chroma_siting_code(siting),
        ];
        let params = self.gateway.buffer_u32(&transfer_params)?;
        let quad_width = output_layout.width() as usize / 2;
        let quad_height = output_layout.height() as usize / 2;
        let groups_x = quad_width.div_ceil(TRANSFER_TILE);
        let groups_y = quad_height.div_ceil(TRANSFER_TILE);
        let group_count = groups_x
            .checked_mul(groups_y)
            .ok_or(MetalError::SizeOverflow("YUV transfer group count"))?;
        let (pipeline, kernel) = match expected_format {
            PixelFormat::Nv12 => (&self.nv12, NV12_KERNEL),
            PixelFormat::P010 => (&self.p010, P010_KERNEL),
            _ => return Err(MetalError::Layout("non-YUV transfer pipeline selected")),
        };
        validate_threadgroup(pipeline, kernel, TRANSFER_TILE, TRANSFER_TILE)?;
        let status =
            self.gateway
                .buffer_zeroed(checked_bytes(group_count, 4, "YUV transfer status")?)?;
        self.gateway.dispatch(
            pipeline,
            &[
                &params,
                &self
                    .raw_surface
                    .as_ref()
                    .ok_or(MetalError::Layout("raw surface disappeared"))?
                    .buffer,
                &self.transfer_table,
                &self
                    .output_surface
                    .as_ref()
                    .ok_or(MetalError::Layout("YUV surface disappeared"))?
                    .buffer,
                &status,
            ],
            Grid::grid_2d(groups_x, groups_y, TRANSFER_TILE, TRANSFER_TILE),
        )?;
        verify_status(&status, group_count, kernel)?;

        let mut frame = FrameBuffer::new(output_layout);
        self.output_surface
            .as_ref()
            .ok_or(MetalError::Layout("YUV surface disappeared"))?
            .buffer
            .read_u8(frame.as_bytes_mut())?;
        let report = self.report(
            &self.raster,
            flat.tile().saturating_mul(flat.tile()),
            upload_bytes
                .checked_add(std::mem::size_of_val(&transfer_params))
                .ok_or(MetalError::SizeOverflow("frame upload count"))?,
            frame.as_bytes().len(),
            raw_reused,
            output_reused,
            started.elapsed(),
            expected_format,
            Some(range),
            Some(siting),
        );
        Ok((frame, report))
    }

    fn dispatch_raster(
        &mut self,
        flat: &FlatFrame,
        raw_bytes: usize,
    ) -> Result<(bool, usize), MetalError> {
        let raw_reused = ensure_surface(self.gateway, &mut self.raw_surface, raw_bytes)?;
        let params_u32 = self.gateway.buffer_u32(&flat.params_u32)?;
        let params_f32 = self.gateway.buffer_f32(&flat.params_f32)?;
        let segments = self.gateway.buffer_f32(nonempty_f32(&flat.segments))?;
        let pieces = self.gateway.buffer_f32(nonempty_f32(&flat.pieces))?;
        let joins = self.gateway.buffer_f32(nonempty_f32(&flat.joins))?;
        let stations = self.gateway.buffer_f32(nonempty_f32(&flat.stations))?;
        let draw_u32 = self.gateway.buffer_u32(nonempty_u32(&flat.draw_u32))?;
        let draw_f32 = self.gateway.buffer_f32(nonempty_f32(&flat.draw_f32))?;
        let styles = self.gateway.buffer_f32(nonempty_f32(&flat.styles))?;
        let tile_offsets = self.gateway.buffer_u32(&flat.tile_offsets)?;
        let tile_draws = self.gateway.buffer_u32(nonempty_u32(&flat.tile_draws))?;
        let tile_flags = self.gateway.buffer_u32(nonempty_u32(&flat.tile_flags))?;
        let group_count = flat
            .cols()
            .checked_mul(flat.rows())
            .ok_or(MetalError::SizeOverflow("raster group count"))?;
        let status = self
            .gateway
            .buffer_zeroed(checked_bytes(group_count, 4, "raster status")?)?;

        let tile = flat.tile();
        validate_threadgroup(&self.raster, RASTER_KERNEL, tile, tile)?;
        self.gateway.dispatch(
            &self.raster,
            &[
                &params_u32,
                &params_f32,
                &segments,
                &pieces,
                &joins,
                &stations,
                &draw_u32,
                &draw_f32,
                &styles,
                &tile_offsets,
                &tile_draws,
                &tile_flags,
                &self
                    .raw_surface
                    .as_ref()
                    .ok_or(MetalError::Layout("raw surface disappeared"))?
                    .buffer,
                &status,
            ],
            Grid::grid_2d(flat.cols(), flat.rows(), tile, tile),
        )?;
        verify_status(&status, group_count, RASTER_KERNEL)?;
        Ok((raw_reused, flat.upload_bytes()?))
    }

    fn dispatch_three_d(
        &mut self,
        flat: &FlatThreeDFrame,
        raw_bytes: usize,
    ) -> Result<(bool, usize), MetalError> {
        let raw_reused = ensure_surface(self.gateway, &mut self.raw_surface, raw_bytes)?;
        let params_u32 = self.gateway.buffer_u32(&flat.params_u32)?;
        let params_f32 = self.gateway.buffer_f32(&flat.params_f32)?;
        let triangles = self.gateway.buffer_f32(nonempty_f32(&flat.triangles))?;
        let draw_u32 = self.gateway.buffer_u32(nonempty_u32(&flat.draw_u32))?;
        let draw_f32 = self.gateway.buffer_f32(nonempty_f32(&flat.draw_f32))?;
        let tile_offsets = self.gateway.buffer_u32(&flat.tile_offsets)?;
        let tile_draws = self.gateway.buffer_u32(nonempty_u32(&flat.tile_draws))?;
        let command_triangles = self.gateway.buffer_u32(&flat.command_triangles)?;
        let tile_triangles = self
            .gateway
            .buffer_u32(nonempty_u32(&flat.tile_triangles))?;
        let texture_u32 = self.gateway.buffer_u32(nonempty_u32(&flat.texture_u32))?;
        let texels_kept = self.reside_texels(flat)?;
        let texels = &self
            .resident_texels
            .as_ref()
            .ok_or(MetalError::Layout("resident texels disappeared"))?
            .buffer;
        let group_count = flat
            .cols()
            .checked_mul(flat.rows())
            .ok_or(MetalError::SizeOverflow("3D raster group count"))?;
        let status =
            self.gateway
                .buffer_zeroed(checked_bytes(group_count, 4, "3D raster status")?)?;

        let tile = flat.tile();
        validate_threadgroup(&self.three_d, THREE_D_KERNEL, tile, tile)?;
        self.gateway.dispatch(
            &self.three_d,
            &[
                &params_u32,
                &params_f32,
                &triangles,
                &draw_u32,
                &draw_f32,
                &tile_offsets,
                &tile_draws,
                &self
                    .raw_surface
                    .as_ref()
                    .ok_or(MetalError::Layout("raw surface disappeared"))?
                    .buffer,
                &status,
                &command_triangles,
                &tile_triangles,
                &texture_u32,
                texels,
            ],
            Grid::grid_2d(flat.cols(), flat.rows(), tile, tile),
        )?;
        verify_status(&status, group_count, THREE_D_KERNEL)?;
        let mut upload_bytes = flat.upload_bytes(texels_kept)?;
        if let Some(bloom) = &flat.bloom {
            let [width, height] = [flat.params_u32[0], flat.params_u32[1]];
            upload_bytes = upload_bytes
                .checked_add(self.bloom_raw_surface(width, height, bloom)?)
                .ok_or(MetalError::SizeOverflow("3D frame upload count"))?;
        }
        Ok((raw_reused, upload_bytes))
    }

    /// Bloom the raw surface in place ([`Bloom`]), in bands of
    /// `bloom_band_rows` as the CPU route does: for each band, the row sums
    /// of the rows it reaches that have none yet into the kept ring, then
    /// the column pass and the mix back into the band's rows. Every kernel
    /// runs the CPU route's arithmetic in its order, all in one command
    /// buffer, in order. Returns the bytes of the bloom's new input buffers
    /// (each band's sizes, the taps and the mix).
    fn bloom_raw_surface(
        &mut self,
        width: u32,
        height: u32,
        bloom: &Bloom,
    ) -> Result<usize, MetalError> {
        let radius =
            u32::try_from(bloom.radius()).map_err(|_| MetalError::SizeOverflow("bloom radius"))?;
        let band = self.bloom_band_rows.max(1);
        let ring = crate::bloom::ring_rows(height, band, bloom.radius());
        let ring_bytes = checked_bytes(
            usize::try_from(u64::from(width) * u64::from(ring))
                .map_err(|_| MetalError::SizeOverflow("bloom row sums"))?,
            BLOOM_SCRATCH_BYTES_PER_PIXEL as usize,
            "bloom row sums",
        )?;
        ensure_surface(self.gateway, &mut self.bloom_sums, ring_bytes)?;
        // Each band: width, height, radius, ring rows; the rows whose sums
        // are taken (first, count, first status word); the band's rows
        // (first, count, first status word).
        let groups_x = (width as usize).div_ceil(TRANSFER_TILE);
        let groups_of = |rows: u32| groups_x * (rows as usize).div_ceil(TRANSFER_TILE);
        let status_word = |groups: usize| {
            u32::try_from(groups).map_err(|_| MetalError::SizeOverflow("bloom status"))
        };
        let mut bands: Vec<[u32; 10]> = Vec::new();
        let (mut summed, mut first, mut groups) = (0_u32, 0_u32, 0_usize);
        while first < height {
            let last = height.min(first.saturating_add(band));
            let ahead = height.min(last.saturating_add(radius));
            let summing = ahead.saturating_sub(summed);
            let rows_status = status_word(groups)?;
            groups += groups_of(summing);
            let band_status = status_word(groups)?;
            groups += groups_of(last - first);
            bands.push([
                width,
                height,
                radius,
                ring,
                summed,
                summing,
                rows_status,
                first,
                last - first,
                band_status,
            ]);
            summed = summed.max(ahead);
            first = last;
        }
        let mix = [bloom.keep(), bloom.strength()];
        let gateway = self.gateway;
        let params = bands
            .iter()
            .map(|sizes| gateway.buffer_u32(sizes))
            .collect::<Result<Vec<_>, _>>()?;
        let taps = gateway.buffer_f32(bloom.taps())?;
        let mix_buffer = gateway.buffer_f32(&mix)?;
        let mut status = gateway.buffer_zeroed(checked_bytes(groups, 4, "bloom status")?)?;
        for (pipeline, name) in [
            (&self.bloom_rows, BLOOM_ROWS_KERNEL),
            (&self.bloom_columns, BLOOM_COLUMNS_KERNEL),
        ] {
            validate_threadgroup(pipeline, name, TRANSFER_TILE, TRANSFER_TILE)?;
        }
        let surface = &mut self
            .raw_surface
            .as_mut()
            .ok_or(MetalError::Layout("raw surface disappeared"))?
            .buffer;
        let sums = &mut self
            .bloom_sums
            .as_mut()
            .ok_or(MetalError::Layout("bloom row sums disappeared"))?
            .buffer;
        let mut batch = gateway.batch();
        let (surface, sums, status_out) = (
            batch.output(surface),
            batch.output(sums),
            batch.output(&mut status),
        );
        let (taps_in, mix_in) = (batch.input(&taps), batch.input(&mix_buffer));
        let grid = |rows: u32| {
            Grid::grid_2d(
                groups_x,
                (rows as usize).div_ceil(TRANSFER_TILE),
                TRANSFER_TILE,
                TRANSFER_TILE,
            )
        };
        for (sizes, buffer) in bands.iter().zip(&params) {
            let sizes_in = batch.input(buffer);
            if sizes[5] > 0 {
                batch.dispatch(
                    &self.bloom_rows,
                    &[
                        sizes_in.read(),
                        taps_in.read(),
                        surface.read(),
                        sums.write(),
                        status_out.write(),
                    ],
                    grid(sizes[5]),
                )?;
            }
            batch.dispatch(
                &self.bloom_columns,
                &[
                    sizes_in.read(),
                    taps_in.read(),
                    mix_in.read(),
                    sums.read(),
                    surface.write(),
                    status_out.write(),
                ],
                grid(sizes[8]),
            )?;
        }
        batch.submit()?;
        verify_status(&status, groups, BLOOM_KERNELS)?;
        Ok(std::mem::size_of_val(bands.as_slice())
            + std::mem::size_of_val(bloom.taps())
            + std::mem::size_of_val(&mix))
    }

    /// Put the frame's texels on the device (FL-033): the last 3D frame's
    /// buffer when the frame samples the same textures (equal content digests
    /// and sizes, in the same order), else a new buffer in its place. The old
    /// buffer is dropped before the new one is made, so the renderer never
    /// holds two. Returns whether the last frame's buffer was kept.
    fn reside_texels(&mut self, flat: &FlatThreeDFrame<'_>) -> Result<bool, MetalError> {
        let key = flat.texel_key();
        if self
            .resident_texels
            .as_ref()
            .is_some_and(|resident| resident.key == key)
        {
            return Ok(true);
        }
        self.resident_texels = None;
        let texels = flat.texels()?;
        let buffer = self.gateway.buffer_f32(nonempty_f32(&texels))?;
        self.resident_texels = Some(ResidentTexels {
            key,
            buffer,
            bytes: checked_bytes(texels.len(), 4, "3D texel bytes")?,
        });
        Ok(false)
    }

    fn transfer_rgba8_surface(
        &mut self,
        width: u32,
        height: u32,
    ) -> Result<(bool, usize), MetalError> {
        let output_layout = FrameLayout::tight(PixelFormat::Rgba8, width, height)?;
        let output_reused =
            ensure_output_surface(self.gateway, &mut self.output_surface, &output_layout)?;
        let transfer_params = [
            width,
            height,
            u32::try_from(output_layout.stride(0))
                .map_err(|_| MetalError::SizeOverflow("RGBA8 stride"))?,
        ];
        let params = self.gateway.buffer_u32(&transfer_params)?;
        let groups_x = (width as usize).div_ceil(TRANSFER_TILE);
        let groups_y = (height as usize).div_ceil(TRANSFER_TILE);
        let group_count = groups_x
            .checked_mul(groups_y)
            .ok_or(MetalError::SizeOverflow("transfer group count"))?;
        validate_threadgroup(&self.rgba8, RGBA8_KERNEL, TRANSFER_TILE, TRANSFER_TILE)?;
        let status =
            self.gateway
                .buffer_zeroed(checked_bytes(group_count, 4, "transfer status")?)?;
        self.gateway.dispatch(
            &self.rgba8,
            &[
                &params,
                &self
                    .raw_surface
                    .as_ref()
                    .ok_or(MetalError::Layout("raw surface disappeared"))?
                    .buffer,
                &self.transfer_table,
                &self
                    .output_surface
                    .as_ref()
                    .ok_or(MetalError::Layout("RGBA8 surface disappeared"))?
                    .buffer,
                &status,
            ],
            Grid::grid_2d(groups_x, groups_y, TRANSFER_TILE, TRANSFER_TILE),
        )?;
        verify_status(&status, group_count, RGBA8_KERNEL)?;

        Ok((output_reused, std::mem::size_of_val(&transfer_params)))
    }

    fn read_rgba8_surface(&self) -> Result<FrameBuffer, MetalError> {
        let surface = self
            .output_surface
            .as_ref()
            .ok_or(MetalError::Layout("RGBA8 surface disappeared"))?;
        let mut frame = FrameBuffer::new(surface.layout.clone());
        surface.buffer.read_u8(frame.as_bytes_mut())?;
        Ok(frame)
    }

    #[allow(clippy::too_many_arguments)]
    fn report(
        &self,
        pipeline: &Pipeline,
        threads_per_threadgroup: usize,
        upload_bytes: usize,
        readback_bytes: usize,
        raw_surface_reused: bool,
        output_surface_reused: bool,
        elapsed: Duration,
        output_format: PixelFormat,
        color_range: Option<ColorRange>,
        chroma_siting: Option<ChromaSiting>,
    ) -> MetalReport {
        MetalReport {
            identity: EngineIdentity::metal(),
            device: self.device.clone(),
            unified_memory: self.unified_memory,
            math_mode: "safe",
            kernel_digest: self.kernel_digest,
            transfer_table_digest: self.transfer_table_digest,
            threads_per_threadgroup,
            max_threads_per_threadgroup: pipeline.max_threads_per_threadgroup(),
            thread_execution_width: pipeline.thread_execution_width(),
            upload_bytes,
            readback_bytes,
            raw_surface_reused,
            output_surface_reused,
            elapsed,
            output_format,
            color_range,
            chroma_siting,
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct Rgba8SurfaceDispatch {
    width: u32,
    height: u32,
    threads_per_threadgroup: usize,
    upload_bytes: usize,
    raw_surface_reused: bool,
    output_surface_reused: bool,
}

#[derive(Debug, Clone, Copy)]
enum NativeRenderPipeline {
    TwoD,
    ThreeD,
}

/// Lumen's main-thread native Metal preview surface.
///
/// This owner couples the renderer's lifetime-held RGBA8 [`SharedBuffer`] to
/// frankentorch's `CAMetalLayer` presenter. No frame-pixel readback occurs on
/// this path. Browser multipart-PNG and terminal protocols are separate,
/// CPU-visible stream consumers and are selected by `fmn-studio` only when
/// native presentation is unavailable or fails.
pub struct NativePreviewRenderer {
    renderer: MetalRenderer,
    presenter: NativePresenter,
}

impl fmt::Debug for NativePreviewRenderer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("NativePreviewRenderer")
            .field("renderer", &self.renderer)
            .field("presentation", &self.presenter.pipeline_info())
            .field("closed", &self.presenter.is_closed())
            .finish()
    }
}

impl NativePreviewRenderer {
    /// Open Lumen and a native `CAMetalLayer` preview window.
    ///
    /// Frankentorch enforces the process-main-thread requirement and retains
    /// every AppKit, Objective-C, drawable, and command-buffer lifetime.
    pub fn new(
        width: u32,
        height: u32,
        title: impl Into<String>,
    ) -> Result<Self, NativePreviewError> {
        let config = PresentationConfig::new(width, height, title);
        config.validate()?;
        let renderer = MetalRenderer::new()?;
        let presenter = NativePresenter::open(&renderer.gateway, config)?;
        Ok(Self {
            renderer,
            presenter,
        })
    }

    /// Fixed occupancy facts for the native drawable transfer pipeline.
    #[must_use]
    pub fn presentation_pipeline_info(&self) -> PresentationPipelineInfo {
        self.presenter.pipeline_info()
    }

    /// Whether the native window has completed teardown.
    #[must_use]
    pub fn is_closed(&self) -> bool {
        self.presenter.is_closed()
    }

    /// Drain pending AppKit events and report the native surface state.
    /// Typed input stays queued for the next [`Self::poll_input`].
    pub fn poll_events(&mut self) -> Result<PresentationState, NativePreviewError> {
        self.presenter.poll_events().map_err(Into::into)
    }

    /// Drain pending AppKit events, append the typed native input queued
    /// since the last drain (pointer, buttons, scroll, keys, modifiers,
    /// focus, capture loss, resize) to `events` in delivery order, and
    /// report the native surface state.
    pub fn poll_input(
        &mut self,
        events: &mut Vec<InputEvent>,
    ) -> Result<PresentationState, NativePreviewError> {
        self.presenter.poll_input(events).map_err(Into::into)
    }

    /// Input events the presenter dropped because its bounded queue was
    /// full between polls.
    #[must_use]
    pub fn input_dropped(&self) -> u64 {
        self.presenter.input_dropped()
    }

    /// Current logical and drawable geometry of the native surface.
    pub fn surface_metrics(&self) -> Result<SurfaceMetrics, NativePreviewError> {
        self.presenter.surface_metrics().map_err(Into::into)
    }

    /// Let the user resize the native window. Render the next frame at the
    /// reported logical size to follow it.
    pub fn set_resizable(&mut self, resizable: bool) -> Result<(), NativePreviewError> {
        self.presenter.set_resizable(resizable).map_err(Into::into)
    }

    /// Choose how rendered frames map to the window. At
    /// [`PixelDensity::Backing`] presenting never resizes the window: render
    /// each frame at [`SurfaceMetrics::backing_size`] for one pixel per
    /// device pixel on a high-DPI screen.
    pub fn set_pixel_density(&mut self, density: PixelDensity) -> Result<(), NativePreviewError> {
        self.presenter
            .set_pixel_density(density)
            .map_err(Into::into)
    }

    /// How rendered frames map to the window.
    #[must_use]
    pub fn pixel_density(&self) -> PixelDensity {
        self.presenter.pixel_density()
    }

    /// Post one real AppKit event for this window into the application
    /// queue (test seam; see [`SyntheticInput`]).
    #[cfg(feature = "synthetic-input")]
    pub fn post_synthetic(&mut self, input: &SyntheticInput) -> Result<(), NativePreviewError> {
        self.presenter.post_synthetic(input).map_err(Into::into)
    }

    /// Render and present a 2D Studio frame without copying frame pixels back
    /// into host-owned memory.
    pub fn render(
        &mut self,
        plan: &RenderPlan,
        mono: &MonoTable,
        binning: &Binning,
        config: FrameConfig,
    ) -> Result<NativePreviewReport, NativePreviewError> {
        let started = Instant::now();
        let dispatch = self
            .renderer
            .dispatch_rgba8_surface(plan, mono, binning, config)?;
        self.present_dispatch(dispatch, NativeRenderPipeline::TwoD, started)
    }

    /// Render and present a prepared 3D Studio frame without frame-pixel
    /// readback.
    pub fn render_three_d(
        &mut self,
        job: &ThreeDJob<'_>,
    ) -> Result<NativePreviewReport, NativePreviewError> {
        let started = Instant::now();
        let dispatch = self.renderer.dispatch_three_d_rgba8_surface(job)?;
        self.present_dispatch(dispatch, NativeRenderPipeline::ThreeD, started)
    }

    /// Idempotently close the native preview surface.
    pub fn close(&mut self) -> Result<(), NativePreviewError> {
        self.presenter.close().map_err(Into::into)
    }

    fn present_dispatch(
        &mut self,
        dispatch: Rgba8SurfaceDispatch,
        pipeline: NativeRenderPipeline,
        started: Instant,
    ) -> Result<NativePreviewReport, NativePreviewError> {
        let surface = self
            .renderer
            .output_surface
            .as_ref()
            .ok_or(MetalError::Layout("RGBA8 surface disappeared"))?;
        if surface.layout.width() != dispatch.width || surface.layout.height() != dispatch.height {
            return Err(MetalError::Layout("RGBA8 presentation dimensions drifted").into());
        }
        let outcome = self.presenter.present_rgba8(
            &surface.buffer,
            dispatch.width,
            dispatch.height,
            surface.layout.stride(0),
        )?;
        let upload_bytes = dispatch
            .upload_bytes
            .checked_add(std::mem::size_of::<[u32; 3]>())
            .ok_or(MetalError::SizeOverflow("native presentation upload count"))?;
        let render_pipeline = match pipeline {
            NativeRenderPipeline::TwoD => &self.renderer.raster,
            NativeRenderPipeline::ThreeD => &self.renderer.three_d,
        };
        let metal = self.renderer.report(
            render_pipeline,
            dispatch.threads_per_threadgroup,
            upload_bytes,
            0,
            dispatch.raw_surface_reused,
            dispatch.output_surface_reused,
            started.elapsed(),
            PixelFormat::Rgba8,
            None,
            None,
        );
        Ok(NativePreviewReport {
            metal,
            presentation: self.presenter.pipeline_info(),
            outcome,
        })
    }
}

struct SurfaceSlot {
    bytes: usize,
    buffer: SharedBuffer,
}

struct OutputSurfaceSlot {
    layout: FrameLayout,
    buffer: SharedBuffer,
}

/// Return whether the existing lifetime-held surface was reused.
fn ensure_surface(
    gateway: Gateway,
    slot: &mut Option<SurfaceSlot>,
    bytes: usize,
) -> Result<bool, MetalError> {
    if slot.as_ref().is_some_and(|surface| surface.bytes == bytes) {
        return Ok(true);
    }
    let buffer = gateway.buffer_zeroed(bytes)?;
    *slot = Some(SurfaceSlot { bytes, buffer });
    Ok(false)
}

/// Return whether the exact negotiated layout's lifetime-held surface was
/// reused. Byte length alone is insufficient: equal-size layouts can put
/// padding at different offsets, and transfer kernels intentionally leave
/// padding untouched.
fn ensure_output_surface(
    gateway: Gateway,
    slot: &mut Option<OutputSurfaceSlot>,
    layout: &FrameLayout,
) -> Result<bool, MetalError> {
    if slot
        .as_ref()
        .is_some_and(|surface| surface.layout == *layout)
    {
        return Ok(true);
    }
    let buffer = gateway.buffer_zeroed(layout.total_bytes())?;
    *slot = Some(OutputSurfaceSlot {
        layout: layout.clone(),
        buffer,
    });
    Ok(false)
}

fn validate_yuv_output(
    layout: &FrameLayout,
    config: FrameConfig,
    expected_format: PixelFormat,
    range: ColorRange,
) -> Result<(), MetalError> {
    if expected_format == PixelFormat::P010 && range != ColorRange::Limited {
        return Err(FrameError::UnsupportedConversion("P010 output is limited-range only").into());
    }
    let expected = match expected_format {
        PixelFormat::Nv12 => "Nv12 destination",
        PixelFormat::P010 => "P010 destination",
        _ => return Err(MetalError::Layout("non-YUV output format selected")),
    };
    if layout.format() != expected_format {
        return Err(FrameError::FormatMismatch {
            expected,
            got: layout.format(),
        }
        .into());
    }
    if layout.width() != config.viewport.width || layout.height() != config.viewport.height {
        return Err(FrameError::DimensionMismatch.into());
    }
    Ok(())
}

fn build_transfer_table() -> (Vec<u32>, Digest) {
    let tables = fmn_frame::transfer::tables();
    let mut bytes = Vec::with_capacity(TRANSFER_TABLE_ENTRIES * 2);
    for bits in 0..=u16::MAX {
        bytes.push(tables.srgb8_from_f16(bits));
    }
    for bits in 0..=u16::MAX {
        bytes.push(tables.linear8_from_f16(bits));
    }
    let digest = sha256(&bytes);
    let (chunks, remainder) = bytes.as_chunks::<4>();
    debug_assert!(remainder.is_empty());
    let words = chunks
        .iter()
        .map(|chunk| u32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]))
        .collect::<Vec<_>>();
    debug_assert_eq!(words.len(), TRANSFER_TABLE_WORDS);
    (words, digest)
}

fn checked_u32(value: usize, name: &'static str) -> Result<u32, MetalError> {
    u32::try_from(value).map_err(|_| MetalError::SizeOverflow(name))
}

const fn color_range_code(range: ColorRange) -> u32 {
    match range {
        ColorRange::Limited => 0,
        ColorRange::Full => 1,
    }
}

const fn chroma_siting_code(siting: ChromaSiting) -> u32 {
    match siting {
        ChromaSiting::Left => 0,
        ChromaSiting::Center => 1,
    }
}

fn validate_threadgroup(
    pipeline: &Pipeline,
    kernel: &'static str,
    x: usize,
    y: usize,
) -> Result<(), MetalError> {
    let requested = x
        .checked_mul(y)
        .ok_or(MetalError::SizeOverflow("threadgroup size"))?;
    let maximum = pipeline.max_threads_per_threadgroup();
    if requested > maximum {
        return Err(MetalError::ThreadgroupTooLarge {
            kernel,
            requested,
            maximum,
        });
    }
    Ok(())
}

fn verify_status(
    status: &SharedBuffer,
    groups: usize,
    kernel: &'static str,
) -> Result<(), MetalError> {
    let mut words = vec![0u32; groups];
    status.read_u32(&mut words)?;
    let completed = words
        .iter()
        .filter(|&&word| word == STATUS_COMPLETE)
        .count();
    if completed != groups {
        return Err(MetalError::IncompleteDispatch {
            kernel,
            completed,
            expected: groups,
        });
    }
    Ok(())
}

fn render_cpu_rgba8(
    plan: &RenderPlan,
    mono: &MonoTable,
    binning: &Binning,
    config: FrameConfig,
    threads: usize,
) -> Result<FrameBuffer, MetalError> {
    let job = FrameJob::with_identity(plan, mono, binning, config, EngineIdentity::fast())?;
    let raw = job.render(threads)?;
    let layout = FrameLayout::tight(
        PixelFormat::Rgba8,
        config.viewport.width,
        config.viewport.height,
    )?;
    let mut output = FrameBuffer::new(layout);
    fmn_frame::convert::rgba16f_to_rgba8(&raw, &mut output)?;
    Ok(output)
}

fn render_cpu_three_d_rgba8(
    job: &ThreeDJob<'_>,
    threads: usize,
) -> Result<FrameBuffer, MetalError> {
    let raw = job.render(threads)?;
    let layout = FrameLayout::tight(
        PixelFormat::Rgba8,
        raw.layout().width(),
        raw.layout().height(),
    )?;
    let mut output = FrameBuffer::new(layout);
    fmn_frame::convert::rgba16f_to_rgba8(&raw, &mut output)?;
    Ok(output)
}

#[derive(Debug)]
struct FlatFrame {
    params_u32: Vec<u32>,
    params_f32: Vec<f32>,
    segments: Vec<f32>,
    pieces: Vec<f32>,
    joins: Vec<f32>,
    stations: Vec<f32>,
    draw_u32: Vec<u32>,
    draw_f32: Vec<f32>,
    styles: Vec<f32>,
    tile_offsets: Vec<u32>,
    tile_draws: Vec<u32>,
    tile_flags: Vec<u32>,
}

impl FlatFrame {
    fn derive(job: &FrameJob<'_>) -> Result<Self, MetalError> {
        let config = job.frame_config();
        let binning = job.frame_binning();
        let tile = binning.tiling().fine_tile.max(1);
        let cols = config.viewport.width.div_ceil(tile);
        let rows = config.viewport.height.div_ceil(tile);
        if binning.tile_count()
            != (cols as usize)
                .checked_mul(rows as usize)
                .ok_or(MetalError::SizeOverflow("tile count"))?
        {
            return Err(MetalError::Layout("binning grid does not match the frame"));
        }
        if binning.offsets().len() != binning.tile_count() + 1
            || binning.draws().len() != binning.flags().len()
        {
            return Err(MetalError::Layout("CSR arrays are not parallel"));
        }

        let samples = match config.aa {
            AaPolicy::Adaptive => 1,
            AaPolicy::Ssaa2x => 2,
            AaPolicy::Ssaa4x => 4,
        };
        let mut flat = Self {
            params_u32: vec![
                config.viewport.width,
                config.viewport.height,
                tile,
                cols,
                samples,
                u32::try_from(job.prepared_draws().len())
                    .map_err(|_| MetalError::SizeOverflow("draw count"))?,
            ],
            params_f32: vec![
                config.background.r as f32,
                config.background.g as f32,
                config.background.b as f32,
                config.background.a as f32,
            ],
            segments: Vec::new(),
            pieces: Vec::new(),
            joins: Vec::new(),
            stations: Vec::new(),
            draw_u32: Vec::with_capacity(job.prepared_draws().len() * DRAW_U32_STRIDE),
            draw_f32: Vec::with_capacity(job.prepared_draws().len() * DRAW_F32_STRIDE),
            styles: Vec::with_capacity(job.prepared_draws().len() * STYLE_STRIDE),
            tile_offsets: binning.offsets().to_vec(),
            tile_draws: binning.draws().to_vec(),
            tile_flags: binning.flags().to_vec(),
        };

        for draw in job.prepared_draws() {
            match draw {
                Some(draw) => flat.push_draw(job, draw)?,
                None => flat.push_empty_draw(),
            }
        }
        flat.validate()?;
        Ok(flat)
    }

    fn push_draw(&mut self, job: &FrameJob<'_>, draw: &Draw) -> Result<(), MetalError> {
        if draw.style.fill_profile.is_some() {
            return Err(MetalError::UnsupportedFillProfile);
        }
        if draw.style.stroke_profile.is_some() {
            return Err(MetalError::UnsupportedStrokeProfile);
        }
        let config = job.frame_config();
        let first_segment = scalar_row(&self.segments, SEGMENT_STRIDE, "segment index")?;
        for segment in job.segments_of(draw) {
            push_segment(
                &mut self.segments,
                segment,
                config,
                draw.translate,
                draw.straight_segments,
            );
        }
        let segment_count = scalar_row(&self.segments, SEGMENT_STRIDE, "segment count")?
            .checked_sub(first_segment)
            .ok_or(MetalError::Layout("segment range reversed"))?;

        let first_piece = scalar_row(&self.pieces, PIECE_STRIDE, "piece index")?;
        for piece in job.pieces_of(draw) {
            self.pieces.extend_from_slice(&[
                (piece.p0[0] + draw.translate[0]) as f32,
                (piece.p0[1] + draw.translate[1]) as f32,
                (piece.p1[0] + draw.translate[0]) as f32,
                (piece.p1[1] + draw.translate[1]) as f32,
                (piece.p2[0] + draw.translate[0]) as f32,
                (piece.p2[1] + draw.translate[1]) as f32,
            ]);
        }
        let piece_count = scalar_row(&self.pieces, PIECE_STRIDE, "piece count")?
            .checked_sub(first_piece)
            .ok_or(MetalError::Layout("piece range reversed"))?;

        let first_join = scalar_row(&self.joins, JOIN_STRIDE, "join index")?;
        for join in job.joins_of(draw) {
            self.joins.extend_from_slice(&[
                join.anchor[0] as f32,
                join.anchor[1] as f32,
                join.t_in[0] as f32,
                join.t_in[1] as f32,
                join.t_out[0] as f32,
                join.t_out[1] as f32,
                join.half_width as f32,
                join.bisector[0] as f32,
                join.bisector[1] as f32,
                join.n_in[0] as f32,
                join.n_in[1] as f32,
                join.n_out[0] as f32,
                join.n_out[1] as f32,
            ]);
        }
        let join_count = scalar_row(&self.joins, JOIN_STRIDE, "join count")?
            .checked_sub(first_join)
            .ok_or(MetalError::Layout("join range reversed"))?;

        let first_station = scalar_row(&self.stations, STATION_STRIDE, "station index")?;
        let mut multi_contour_gradient = false;
        if let Some(field) = job.field_of(draw) {
            let (points, params, next, edge_params) = field.stations();
            if points.len() != params.len() {
                return Err(MetalError::Layout("gradient stations are not parallel"));
            }
            multi_contour_gradient = field.has_explicit_contours();
            if multi_contour_gradient
                && (points.len() != next.len() || points.len() != edge_params.len())
            {
                return Err(MetalError::Layout(
                    "gradient contour edges are not parallel",
                ));
            }
            for (offset, (point, &param)) in points.iter().zip(params).enumerate() {
                let local_next = if multi_contour_gradient {
                    next[offset]
                } else if offset + 1 < points.len() {
                    offset + 1
                } else {
                    0
                };
                if local_next >= points.len() {
                    return Err(MetalError::Layout("gradient contour edge escapes draw"));
                }
                let local_next = u32::try_from(local_next)
                    .map_err(|_| MetalError::SizeOverflow("gradient next station"))?;
                let global_next = first_station
                    .checked_add(local_next)
                    .ok_or(MetalError::SizeOverflow("gradient next station"))?;
                let edge_param = if multi_contour_gradient {
                    edge_params[offset]
                } else if offset + 1 < params.len() {
                    params[offset + 1]
                } else {
                    1.0
                };
                self.stations.extend_from_slice(&[
                    (point[0] + draw.translate[0]) as f32,
                    (point[1] + draw.translate[1]) as f32,
                    param as f32,
                    f32::from_bits(global_next),
                    edge_param as f32,
                ]);
            }
        }
        let station_count = scalar_row(&self.stations, STATION_STRIDE, "station count")?
            .checked_sub(first_station)
            .ok_or(MetalError::Layout("station range reversed"))?;

        let mut flags = 0;
        if draw.draws_fill {
            flags |= DRAW_FILL;
        }
        if draw.draws_stroke {
            flags |= DRAW_STROKE;
        }
        if draw.style.stroke_behind {
            flags |= STROKE_BEHIND;
        }
        if draw.flat_fill.is_some() {
            flags |= FLAT_FILL;
        }
        if multi_contour_gradient {
            flags |= MULTI_CONTOUR_GRADIENT;
        }
        let joint = match draw.style.joint_type {
            JointType::Auto | JointType::NoJoint => 0,
            JointType::Bevel => 1,
            JointType::Miter => 2,
        };
        self.draw_u32.extend_from_slice(&[
            first_segment,
            segment_count,
            first_piece,
            piece_count,
            first_join,
            join_count,
            first_station,
            station_count,
            flags,
            joint,
        ]);
        let stroke_slab = draw.stroke.map_or([0.0; 4], |stroke| stroke.slab);
        self.draw_f32.extend(
            draw.fill_slab
                .into_iter()
                .chain(stroke_slab)
                .map(|value| value as f32),
        );
        self.styles.extend_from_slice(&[
            draw.style.fill_rgba[0],
            draw.style.fill_rgba[1],
            draw.style.fill_rgba[2],
            draw.style.fill_rgba[3],
            draw.style.fill_rgba_end[0],
            draw.style.fill_rgba_end[1],
            draw.style.fill_rgba_end[2],
            draw.style.fill_rgba_end[3],
            draw.style.stroke_rgba[0],
            draw.style.stroke_rgba[1],
            draw.style.stroke_rgba[2],
            draw.style.stroke_rgba[3],
            draw.style.stroke_rgba_end[0],
            draw.style.stroke_rgba_end[1],
            draw.style.stroke_rgba_end[2],
            draw.style.stroke_rgba_end[3],
            crate::stroke::width_px(draw.style.stroke_width, config.map) as f32,
            crate::stroke::width_px(draw.style.stroke_width_end, config.map) as f32,
            crate::fill::border_width_px(draw.style.fill_border_width, config.map) as f32,
            draw.style.anti_alias_width,
        ]);
        Ok(())
    }

    fn push_empty_draw(&mut self) {
        self.draw_u32
            .extend(std::iter::repeat_n(0, DRAW_U32_STRIDE));
        self.draw_f32
            .extend(std::iter::repeat_n(0.0, DRAW_F32_STRIDE));
        self.styles.extend(std::iter::repeat_n(0.0, STYLE_STRIDE));
    }

    fn validate(&self) -> Result<(), MetalError> {
        for (slice, stride, name) in [
            (self.segments.as_slice(), SEGMENT_STRIDE, "segments"),
            (self.pieces.as_slice(), PIECE_STRIDE, "pieces"),
            (self.joins.as_slice(), JOIN_STRIDE, "joins"),
            (self.stations.as_slice(), STATION_STRIDE, "stations"),
            (self.draw_f32.as_slice(), DRAW_F32_STRIDE, "draw_f32"),
            (self.styles.as_slice(), STYLE_STRIDE, "styles"),
        ] {
            if !slice.len().is_multiple_of(stride) {
                return Err(MetalError::Layout(name));
            }
        }
        if !self.draw_u32.len().is_multiple_of(DRAW_U32_STRIDE) {
            return Err(MetalError::Layout("draw_u32"));
        }
        let draws = self.draw_u32.len() / DRAW_U32_STRIDE;
        if self.draw_f32.len() / DRAW_F32_STRIDE != draws
            || self.styles.len() / STYLE_STRIDE != draws
            || self.params_u32.get(5).copied() != u32::try_from(draws).ok()
        {
            return Err(MetalError::Layout("draw tables are not parallel"));
        }
        Ok(())
    }

    fn tile(&self) -> usize {
        self.params_u32[2] as usize
    }

    fn cols(&self) -> usize {
        self.params_u32[3] as usize
    }

    fn rows(&self) -> usize {
        (self.params_u32[1] as usize).div_ceil(self.tile())
    }

    fn upload_bytes(&self) -> Result<usize, MetalError> {
        let scalars = [
            self.params_u32.len(),
            self.params_f32.len(),
            self.segments.len(),
            self.pieces.len(),
            self.joins.len(),
            self.stations.len(),
            self.draw_u32.len(),
            self.draw_f32.len(),
            self.styles.len(),
            self.tile_offsets.len(),
            self.tile_draws.len(),
            self.tile_flags.len(),
        ]
        .into_iter()
        .try_fold(0usize, |sum, len| sum.checked_add(len))
        .ok_or(MetalError::SizeOverflow("frame upload scalar count"))?;
        checked_bytes(scalars, 4, "frame upload bytes")
    }
}

#[derive(Debug)]
struct FlatThreeDFrame<'a> {
    params_u32: Vec<u32>,
    params_f32: Vec<f32>,
    triangles: Vec<f32>,
    draw_u32: Vec<u32>,
    draw_f32: Vec<f32>,
    tile_offsets: Vec<u32>,
    tile_draws: Vec<u32>,
    /// CSR over `tile_draws`: command `c`'s triangles are
    /// `tile_triangles[command_triangles[c]..command_triangles[c + 1]]`.
    command_triangles: Vec<u32>,
    tile_triangles: Vec<u32>,
    /// One row per textured draw: the light texture's first texel, width and
    /// height; the dark texture's (first texel `THREE_D_NO_TEXTURE` when
    /// none); the wrap codes (`u | v << 4`); and whether zero alpha discards.
    texture_u32: Vec<u32>,
    /// Every texture the frame samples, once each, in the order the rows
    /// first name them. Their premultiplied linear texels follow one another
    /// in the texel buffer ([`FlatThreeDFrame::texels`]), which a renderer
    /// keeps for the next frame that samples the same textures.
    textures: Vec<&'a Texture>,
    /// The texels of `textures`.
    texel_count: usize,
    /// The job's bloom, run on the raw surface after the raster.
    bloom: Option<Bloom>,
}

/// The kernel's code for a wrap mode.
const fn three_d_wrap_code(wrap: TextureWrap) -> u32 {
    match wrap {
        TextureWrap::Repeat => 0,
        TextureWrap::ClampToEdge => 1,
        TextureWrap::MirroredRepeat => 2,
    }
}

/// Which of a draw's triangles a tile command lists.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TriangleBins {
    /// Only the triangles whose f32 pixel bounds meet the tile.
    PerTile,
    /// Every triangle of the draw, in every tile the draw is binned to (the
    /// kernel's behaviour before per-tile lists; kept as the test reference).
    #[cfg_attr(not(test), allow(dead_code))]
    Whole,
}

impl<'a> FlatThreeDFrame<'a> {
    fn derive(job: &ThreeDJob<'a>) -> Result<Self, MetalError> {
        Self::derive_with(job, TriangleBins::PerTile)
    }

    fn derive_with(job: &ThreeDJob<'a>, bins: TriangleBins) -> Result<Self, MetalError> {
        let camera = job.camera();
        let width = camera.pixel_width();
        let height = camera.pixel_height();
        let tile = job.tiling().fine_tile.max(1);
        let cols = width.div_ceil(tile);
        let sample_grid = job.sample_grid();
        if !matches!(sample_grid, 1 | 2 | 4) {
            return Err(MetalError::Layout("unsupported 3D sample grid"));
        }
        let draw_count = u32::try_from(job.prepared_draws().len())
            .map_err(|_| MetalError::SizeOverflow("3D draw count"))?;
        let background = camera.background();
        let mut flat = Self {
            params_u32: vec![width, height, tile, cols, sample_grid, draw_count],
            params_f32: vec![
                background.r as f32,
                background.g as f32,
                background.b as f32,
                background.a as f32,
            ],
            triangles: Vec::new(),
            draw_u32: Vec::with_capacity(job.prepared_draws().len() * THREE_D_DRAW_U32_STRIDE),
            draw_f32: Vec::with_capacity(job.prepared_draws().len() * THREE_D_DRAW_F32_STRIDE),
            tile_offsets: Vec::new(),
            tile_draws: Vec::new(),
            command_triangles: Vec::new(),
            tile_triangles: Vec::new(),
            texture_u32: Vec::new(),
            textures: Vec::new(),
            texel_count: 0,
            bloom: job.bloom().cloned(),
        };
        let mut draw_bounds = Vec::with_capacity(job.prepared_draws().len());
        let mut uploaded = std::collections::HashMap::new();
        // The frame's triangle rows are reserved once, at their exact count,
        // instead of growing a triangle at a time (a frame of 50,000 dots
        // holds about 22 MB of them). The rows are unchanged.
        let triangle_scalars = job
            .prepared_draws()
            .iter()
            .map(|draw| match &draw.primitive {
                CompiledPrimitive::Triangles { triangles, .. } => triangles.len(),
                CompiledPrimitive::Vector(_) => 0,
            })
            .try_fold(0_usize, usize::checked_add)
            .and_then(|triangles| triangles.checked_mul(THREE_D_TRIANGLE_STRIDE))
            .ok_or(MetalError::SizeOverflow("3D triangles"))?;
        flat.triangles
            .try_reserve_exact(triangle_scalars)
            .map_err(|_| MetalError::SizeOverflow("3D triangles"))?;

        for (draw_index, draw) in job.prepared_draws().iter().enumerate() {
            let CompiledPrimitive::Triangles { triangles, shader } = &draw.primitive else {
                return Err(MetalError::UnsupportedThreeDPrimitive {
                    draw: draw_index,
                    primitive: "retained-vector",
                });
            };
            let (shader_code, texture_row) = match *shader {
                Shader::Gouraud => (THREE_D_SHADER_GOURAUD, THREE_D_NO_TEXTURE),
                Shader::Dot { .. } => (THREE_D_SHADER_DOT, THREE_D_NO_TEXTURE),
                Shader::Texture { material, .. } => (
                    THREE_D_SHADER_TEXTURE,
                    flat.push_texture_row(material, &mut uploaded)?,
                ),
            };

            let first_triangle = scalar_row(
                &flat.triangles,
                THREE_D_TRIANGLE_STRIDE,
                "3D triangle index",
            )?;
            for triangle in triangles {
                push_three_d_triangle(&mut flat.triangles, triangle);
            }
            let triangle_count = scalar_row(
                &flat.triangles,
                THREE_D_TRIANGLE_STRIDE,
                "3D triangle count",
            )?
            .checked_sub(first_triangle)
            .ok_or(MetalError::Layout("3D triangle range reversed"))?;
            flat.draw_u32.extend_from_slice(&[
                first_triangle,
                triangle_count,
                shader_code,
                u32::from(draw.depth_test),
                texture_row,
            ]);
            flat.draw_f32
                .extend_from_slice(&three_d_shader_slab(*shader));
            draw_bounds.push(draw.bounds);
        }

        (flat.tile_offsets, flat.tile_draws) =
            stable_three_d_bins(&draw_bounds, width, height, tile)?;
        (flat.command_triangles, flat.tile_triangles) = three_d_tile_triangles(
            &flat.triangles,
            &flat.draw_u32,
            &flat.tile_offsets,
            &flat.tile_draws,
            [width, height, tile],
            bins,
        )?;
        flat.validate()?;
        Ok(flat)
    }

    /// A textured draw's row, uploading each of its textures the first time
    /// the frame meets it.
    fn push_texture_row(
        &mut self,
        material: TextureMaterial<'a>,
        uploaded: &mut std::collections::HashMap<*const Texture, [u32; 3]>,
    ) -> Result<u32, MetalError> {
        let light = self.upload_texture(material.light, uploaded)?;
        let dark = match material.dark {
            Some(dark) => self.upload_texture(dark, uploaded)?,
            None => [THREE_D_NO_TEXTURE, 0, 0],
        };
        let row = u32::try_from(self.texture_u32.len() / THREE_D_TEXTURE_U32_STRIDE)
            .map_err(|_| MetalError::SizeOverflow("3D texture row"))?;
        self.texture_u32.extend_from_slice(&[
            light[0],
            light[1],
            light[2],
            dark[0],
            dark[1],
            dark[2],
            three_d_wrap_code(material.sampler.wrap_u)
                | (three_d_wrap_code(material.sampler.wrap_v) << 4),
            u32::from(material.discard_transparent),
        ]);
        Ok(row)
    }

    /// The first texel, width and height of `texture` in the frame's texels.
    fn upload_texture(
        &mut self,
        texture: &'a Texture,
        uploaded: &mut std::collections::HashMap<*const Texture, [u32; 3]>,
    ) -> Result<[u32; 3], MetalError> {
        let key: *const Texture = texture;
        if let Some(&placed) = uploaded.get(&key) {
            return Ok(placed);
        }
        let first = u32::try_from(self.texel_count)
            .map_err(|_| MetalError::SizeOverflow("3D texel index"))?;
        self.texel_count = self
            .texel_count
            .checked_add(texture.premultiplied_texels().len())
            .ok_or(MetalError::SizeOverflow("3D texels"))?;
        self.textures.push(texture);
        let placed = [first, texture.width(), texture.height()];
        uploaded.insert(key, placed);
        Ok(placed)
    }

    /// Every texture's premultiplied linear texels, one after another in the
    /// order the rows place them, four scalars a texel: what the kernel
    /// samples.
    fn texels(&self) -> Result<Vec<f32>, MetalError> {
        let scalars = self
            .texel_count
            .checked_mul(4)
            .ok_or(MetalError::SizeOverflow("3D texels"))?;
        let mut texels = Vec::new();
        texels
            .try_reserve_exact(scalars)
            .map_err(|_| MetalError::SizeOverflow("3D texels"))?;
        for texture in &self.textures {
            for texel in texture.premultiplied_texels() {
                texels.extend_from_slice(texel);
            }
        }
        if texels.iter().any(|value| !value.is_finite()) {
            return Err(MetalError::Layout(
                "3D f32 packet contains a non-finite value",
            ));
        }
        Ok(texels)
    }

    /// What the frame's texels are: each texture's content digest and size,
    /// in order. Frames with equal keys sample equal texels.
    fn texel_key(&self) -> Vec<(Digest, u32, u32)> {
        self.textures
            .iter()
            .map(|texture| (texture.digest(), texture.width(), texture.height()))
            .collect()
    }

    fn validate(&self) -> Result<(), MetalError> {
        if self.params_u32.len() != 6 || self.params_f32.len() != 4 {
            return Err(MetalError::Layout("3D frame parameters"));
        }
        if !self.triangles.len().is_multiple_of(THREE_D_TRIANGLE_STRIDE)
            || !self.draw_u32.len().is_multiple_of(THREE_D_DRAW_U32_STRIDE)
            || !self.draw_f32.len().is_multiple_of(THREE_D_DRAW_F32_STRIDE)
        {
            return Err(MetalError::Layout("3D scalar-table stride"));
        }
        if self
            .params_f32
            .iter()
            .chain(&self.triangles)
            .chain(&self.draw_f32)
            .any(|value| !value.is_finite())
        {
            return Err(MetalError::Layout(
                "3D f32 packet contains a non-finite value",
            ));
        }
        self.validate_textures()?;
        let draws = self.draw_u32.len() / THREE_D_DRAW_U32_STRIDE;
        if self.draw_f32.len() / THREE_D_DRAW_F32_STRIDE != draws
            || self.params_u32.get(5).copied() != u32::try_from(draws).ok()
        {
            return Err(MetalError::Layout("3D draw tables are not parallel"));
        }
        let tile_count = self
            .cols()
            .checked_mul(self.rows())
            .ok_or(MetalError::SizeOverflow("3D tile count"))?;
        if self.tile_offsets.len() != tile_count + 1
            || self.tile_offsets.first().copied() != Some(0)
            || self.tile_offsets.last().copied() != u32::try_from(self.tile_draws.len()).ok()
            || self.tile_offsets.windows(2).any(|pair| pair[0] > pair[1])
            || self.tile_draws.iter().any(|&draw| draw as usize >= draws)
        {
            return Err(MetalError::Layout("3D stable tile CSR"));
        }
        // Every listed triangle is its command's draw's, ascending.
        if self.command_triangles.len() != self.tile_draws.len() + 1
            || self.command_triangles.first().copied() != Some(0)
            || self.command_triangles.last().copied()
                != u32::try_from(self.tile_triangles.len()).ok()
            || self
                .command_triangles
                .windows(2)
                .any(|pair| pair[0] > pair[1])
        {
            return Err(MetalError::Layout("3D tile triangle CSR"));
        }
        let ranges = self.draw_u32.as_chunks::<THREE_D_DRAW_U32_STRIDE>().0;
        for (command, &draw) in self.tile_draws.iter().enumerate() {
            let [first, count, ..] = ranges[draw as usize];
            let listed = &self.tile_triangles[self.command_triangles[command] as usize
                ..self.command_triangles[command + 1] as usize];
            if listed.windows(2).any(|pair| pair[0] >= pair[1])
                || listed
                    .iter()
                    .any(|&triangle| triangle < first || triangle - first >= count)
            {
                return Err(MetalError::Layout("3D tile triangle outside its draw"));
            }
        }
        Ok(())
    }

    /// Every textured draw names a texture row, every other draw none, and
    /// every row's textures lie inside the frame's texels with the kernel's
    /// wrap codes; the texel index of every scalar fits the kernel's `uint`.
    fn validate_textures(&self) -> Result<(), MetalError> {
        if !self
            .texture_u32
            .len()
            .is_multiple_of(THREE_D_TEXTURE_U32_STRIDE)
            || self
                .texel_count
                .checked_mul(4)
                .and_then(|scalars| u32::try_from(scalars).ok())
                .is_none()
        {
            return Err(MetalError::Layout("3D texture tables"));
        }
        let rows = self.texture_u32.len() / THREE_D_TEXTURE_U32_STRIDE;
        for draw in self.draw_u32.as_chunks::<THREE_D_DRAW_U32_STRIDE>().0 {
            let textured = draw[2] == THREE_D_SHADER_TEXTURE;
            let named = draw[4] != THREE_D_NO_TEXTURE;
            if textured != named || (named && draw[4] as usize >= rows) {
                return Err(MetalError::Layout("3D draw texture row"));
            }
        }
        let texel_count = self.texel_count;
        let inside = |first: u32, width: u32, height: u32| {
            width > 0
                && height > 0
                && width <= i32::MAX as u32 / 2
                && height <= i32::MAX as u32 / 2
                && (width as usize)
                    .checked_mul(height as usize)
                    .and_then(|count| count.checked_add(first as usize))
                    .is_some_and(|end| end <= texel_count)
        };
        for row in self.texture_u32.as_chunks::<THREE_D_TEXTURE_U32_STRIDE>().0 {
            let [
                light,
                light_width,
                light_height,
                dark,
                dark_width,
                dark_height,
                wrap,
                discard,
            ] = *row;
            if !inside(light, light_width, light_height)
                || (dark != THREE_D_NO_TEXTURE && !inside(dark, dark_width, dark_height))
                || wrap & 0xf > 2
                || wrap >> 4 > 2
                || discard > 1
            {
                return Err(MetalError::Layout("3D texture row"));
            }
        }
        Ok(())
    }

    fn tile(&self) -> usize {
        self.params_u32[2] as usize
    }

    fn cols(&self) -> usize {
        self.params_u32[3] as usize
    }

    fn rows(&self) -> usize {
        (self.params_u32[1] as usize).div_ceil(self.tile())
    }

    /// The bytes the frame copies into new buffers: every table, and the
    /// texels unless the renderer kept them from an earlier frame.
    fn upload_bytes(&self, texels_kept: bool) -> Result<usize, MetalError> {
        let texels = if texels_kept {
            0
        } else {
            self.texel_count
                .checked_mul(4)
                .ok_or(MetalError::SizeOverflow("3D upload scalar count"))?
        };
        let scalars = [
            self.params_u32.len(),
            self.params_f32.len(),
            self.triangles.len(),
            self.draw_u32.len(),
            self.draw_f32.len(),
            self.tile_offsets.len(),
            self.tile_draws.len(),
            self.command_triangles.len(),
            self.tile_triangles.len(),
            self.texture_u32.len(),
            texels,
        ]
        .into_iter()
        .try_fold(0usize, |sum, len| sum.checked_add(len))
        .ok_or(MetalError::SizeOverflow("3D upload scalar count"))?;
        checked_bytes(scalars, 4, "3D upload bytes")
    }
}

/// The last 3D frame's texels on the device, and what they are (FL-033):
/// each texture's content digest and size, in order.
struct ResidentTexels {
    key: Vec<(Digest, u32, u32)>,
    buffer: SharedBuffer,
    bytes: usize,
}

fn push_three_d_triangle(out: &mut Vec<f32>, triangle: &crate::three_d::RasterTriangle) {
    for vertex in triangle.vertices {
        out.extend_from_slice(&[
            vertex.screen[0] as f32,
            vertex.screen[1] as f32,
            vertex.inverse_w as f32,
            vertex.ndc_z as f32,
            vertex.attributes.world[0] as f32,
            vertex.attributes.world[1] as f32,
            vertex.attributes.world[2] as f32,
            vertex.attributes.normal[0] as f32,
            vertex.attributes.normal[1] as f32,
            vertex.attributes.normal[2] as f32,
            vertex.attributes.color[0] as f32,
            vertex.attributes.color[1] as f32,
            vertex.attributes.color[2] as f32,
            vertex.attributes.color[3] as f32,
            vertex.attributes.uv[0] as f32,
            vertex.attributes.uv[1] as f32,
            vertex.attributes.opacity as f32,
        ]);
    }
}

fn three_d_shader_slab(shader: Shader<'_>) -> [f32; THREE_D_DRAW_F32_STRIDE] {
    let mut slab = [0.0; THREE_D_DRAW_F32_STRIDE];
    if let Shader::Texture {
        shading,
        light_position,
        camera_position,
        ..
    } = shader
    {
        slab[0..3].copy_from_slice(&shading.map(|value| value as f32));
        slab[3..6].copy_from_slice(&light_position.map(|value| value as f32));
        slab[6..9].copy_from_slice(&camera_position.map(|value| value as f32));
    }
    if let Shader::Dot {
        center,
        radius,
        color,
        glow_factor,
        shading,
        to_camera,
        scaled_aa_width,
        light_position,
        camera_position,
    } = shader
    {
        slab[0..3].copy_from_slice(&center.map(|value| value as f32));
        slab[3] = radius as f32;
        slab[4..8].copy_from_slice(&[
            color.r as f32,
            color.g as f32,
            color.b as f32,
            color.a as f32,
        ]);
        slab[8] = glow_factor as f32;
        slab[9..12].copy_from_slice(&shading.map(|value| value as f32));
        slab[12..15].copy_from_slice(&to_camera.map(|value| value as f32));
        slab[15] = scaled_aa_width as f32;
        slab[16..19].copy_from_slice(&light_position.map(|value| value as f32));
        slab[19..22].copy_from_slice(&camera_position.map(|value| value as f32));
    }
    slab
}

/// The half-open pixel span `[floor(min), ceil(max))` of three f32 corner
/// coordinates, exactly as `three_d_pixel_in_triangle_bounds` computes it on
/// the device (f32 min and max are exact, floor and ceil of an f32 are exact).
fn three_d_pixel_span(a: f32, b: f32, c: f32) -> (f32, f32) {
    (a.min(b.min(c)).floor(), a.max(b.max(c)).ceil())
}

/// The first and last (inclusive) tile along one axis whose pixels the span
/// `[lo, hi)` meets, within `0..extent` pixels; `None` when it meets none.
fn three_d_tile_range(lo: f32, hi: f32, extent: u32, tile: u32) -> Option<(usize, usize)> {
    // `lo` and `hi` are integral f32 values; every pixel index below `extent`
    // is exact in f32, so these comparisons are the kernel's.
    let extent_f = extent as f32;
    if !(hi > 0.0 && lo < extent_f && lo < hi) {
        return None;
    }
    let first = if lo <= 0.0 { 0 } else { lo as u32 };
    let end = if hi >= extent_f { extent } else { hi as u32 };
    if first >= end {
        return None;
    }
    Some(((first / tile) as usize, ((end - 1) / tile) as usize))
}

/// Each tile command's triangle list (see [`FlatThreeDFrame`]).
///
/// The kernel runs both of its per-triangle tests (the supersampling boundary
/// and the shading pass) only for pixels inside the triangle's f32 pixel
/// span, `three_d_pixel_in_triangle_bounds`. A triangle whose span misses
/// every pixel of a tile therefore changes nothing there and is left out of
/// that tile's lists. The kept triangles stay in their draw's order, and the
/// tile's draws are `tile_draws` unchanged, so every pixel sees the same
/// sequence of triangles that can touch it as with whole draws.
fn three_d_tile_triangles(
    triangles: &[f32],
    draw_u32: &[u32],
    tile_offsets: &[u32],
    tile_draws: &[u32],
    [width, height, tile]: [u32; 3],
    bins: TriangleBins,
) -> Result<(Vec<u32>, Vec<u32>), MetalError> {
    let ranges = draw_u32.as_chunks::<THREE_D_DRAW_U32_STRIDE>().0;
    let tile_count = tile_offsets.len().saturating_sub(1);
    // The (tile, command) pairs of each draw, in tile order, as one CSR:
    // draw `d`'s pairs are `pairs[draw_starts[d]..draw_starts[d + 1]]`.
    let mut draw_starts = vec![0_usize; ranges.len() + 1];
    for tile_index in 0..tile_count {
        for &draw in three_d_tile_commands(tile_offsets, tile_draws, tile_index)? {
            if draw as usize >= ranges.len() {
                return Err(MetalError::Layout("3D tile command names no draw"));
            }
            draw_starts[draw as usize + 1] += 1;
        }
    }
    for draw in 0..ranges.len() {
        draw_starts[draw + 1] += draw_starts[draw];
    }
    let mut cursors = draw_starts[..ranges.len()].to_vec();
    let mut pairs = vec![(0_usize, 0_usize); draw_starts[ranges.len()]];
    for tile_index in 0..tile_count {
        let lo = tile_offsets[tile_index] as usize;
        let commands = three_d_tile_commands(tile_offsets, tile_draws, tile_index)?;
        for (offset, &draw) in commands.iter().enumerate() {
            let cursor = &mut cursors[draw as usize];
            pairs[*cursor] = (tile_index, lo + offset);
            *cursor += 1;
        }
    }

    // Count each command's triangles, then write them at the command's
    // offset in the same order: no list per command, and one copy.
    let draws = ThreeDDrawPairs {
        ranges,
        starts: &draw_starts,
        pairs: &pairs,
    };
    let mut command_of_tile = vec![usize::MAX; tile_count];
    let mut counts = vec![0_usize; tile_draws.len()];
    visit_three_d_tile_triangles(
        triangles,
        &draws,
        &mut command_of_tile,
        [width, height, tile],
        bins,
        |command, span| counts[command] = counts[command].saturating_add(span.len()),
    )?;
    let mut offsets = Vec::with_capacity(counts.len() + 1);
    offsets.push(0);
    let mut total = 0_usize;
    for &count in &counts {
        total = total.saturating_add(count);
        offsets.push(
            u32::try_from(total)
                .map_err(|_| MetalError::SizeOverflow("3D tile triangle offset"))?,
        );
    }
    let mut cursors = offsets[..counts.len()]
        .iter()
        .map(|&offset| offset as usize)
        .collect::<Vec<_>>();
    let mut flat = vec![0_u32; total];
    visit_three_d_tile_triangles(
        triangles,
        &draws,
        &mut command_of_tile,
        [width, height, tile],
        bins,
        |command, span| {
            let cursor = &mut cursors[command];
            for triangle in span {
                flat[*cursor] = triangle;
                *cursor += 1;
            }
        },
    )?;
    Ok((offsets, flat))
}

/// Tile `tile_index`'s commands: the draws it lists, in order.
fn three_d_tile_commands<'a>(
    tile_offsets: &[u32],
    tile_draws: &'a [u32],
    tile_index: usize,
) -> Result<&'a [u32], MetalError> {
    let lo = tile_offsets[tile_index] as usize;
    let hi = tile_offsets[tile_index + 1] as usize;
    tile_draws
        .get(lo..hi)
        .ok_or(MetalError::Layout("3D tile command range"))
}

/// Every draw's triangle range and its (tile, command) pairs in tile order.
struct ThreeDDrawPairs<'a> {
    ranges: &'a [[u32; THREE_D_DRAW_U32_STRIDE]],
    starts: &'a [usize],
    pairs: &'a [(usize, usize)],
}

/// Visit each command's triangles in list order: draw by draw, and within a
/// draw triangle by triangle, the commands of the tiles its f32 pixel span
/// meets (`PerTile`), or the draw's whole range at each of its commands
/// (`Whole`). `command_of_tile` is all `usize::MAX` on entry and on return.
fn visit_three_d_tile_triangles(
    triangles: &[f32],
    draws: &ThreeDDrawPairs<'_>,
    command_of_tile: &mut [usize],
    [width, height, tile]: [u32; 3],
    bins: TriangleBins,
    mut visit: impl FnMut(usize, std::ops::Range<u32>),
) -> Result<(), MetalError> {
    let cols = width.div_ceil(tile) as usize;
    for (draw, &[first, count, ..]) in draws.ranges.iter().enumerate() {
        let pairs = &draws.pairs[draws.starts[draw]..draws.starts[draw + 1]];
        if pairs.is_empty() {
            continue;
        }
        let last = first
            .checked_add(count)
            .ok_or(MetalError::SizeOverflow("3D draw triangle range"))?;
        if bins == TriangleBins::Whole {
            for &(_, command) in pairs {
                visit(command, first..last);
            }
            continue;
        }
        for &(tile_index, command) in pairs {
            command_of_tile[tile_index] = command;
        }
        for triangle in first..last {
            let corner = |index: usize, axis: usize| -> Result<f32, MetalError> {
                triangles
                    .get(
                        triangle as usize * THREE_D_TRIANGLE_STRIDE
                            + index * THREE_D_VERTEX_STRIDE
                            + axis,
                    )
                    .copied()
                    .filter(|value| value.is_finite())
                    .ok_or(MetalError::Layout("3D triangle corner"))
            };
            let (x_lo, x_hi) = three_d_pixel_span(corner(0, 0)?, corner(1, 0)?, corner(2, 0)?);
            let (y_lo, y_hi) = three_d_pixel_span(corner(0, 1)?, corner(1, 1)?, corner(2, 1)?);
            let (Some((col_lo, col_hi)), Some((row_lo, row_hi))) = (
                three_d_tile_range(x_lo, x_hi, width, tile),
                three_d_tile_range(y_lo, y_hi, height, tile),
            ) else {
                continue;
            };
            for row in row_lo..=row_hi {
                for col in col_lo..=col_hi {
                    let command = command_of_tile[row * cols + col];
                    if command != usize::MAX {
                        visit(command, triangle..triangle + 1);
                    }
                }
            }
        }
        for &(tile_index, _) in pairs {
            command_of_tile[tile_index] = usize::MAX;
        }
    }
    Ok(())
}

fn stable_three_d_bins(
    bounds: &[Option<[f64; 4]>],
    width: u32,
    height: u32,
    tile: u32,
) -> Result<(Vec<u32>, Vec<u32>), MetalError> {
    let cols = width.div_ceil(tile) as usize;
    let rows = height.div_ceil(tile) as usize;
    let tile_count = cols
        .checked_mul(rows)
        .ok_or(MetalError::SizeOverflow("3D tile count"))?;
    let mut counts = vec![0usize; tile_count];
    for &draw_bounds in bounds {
        visit_three_d_tiles(draw_bounds, width, height, tile, |index| {
            counts[index] = counts[index]
                .checked_add(1)
                .ok_or(MetalError::SizeOverflow("3D tile command count"))?;
            Ok(())
        })?;
    }

    let mut tile_offsets = Vec::with_capacity(tile_count + 1);
    tile_offsets.push(0);
    let mut total = 0usize;
    for &count in &counts {
        total = total
            .checked_add(count)
            .ok_or(MetalError::SizeOverflow("3D tile command total"))?;
        tile_offsets.push(
            u32::try_from(total).map_err(|_| MetalError::SizeOverflow("3D tile command offset"))?,
        );
    }
    let mut tile_draws = vec![0u32; total];
    let mut cursors = tile_offsets[..tile_count]
        .iter()
        .map(|&offset| offset as usize)
        .collect::<Vec<_>>();
    for (draw, &draw_bounds) in bounds.iter().enumerate() {
        let draw = u32::try_from(draw).map_err(|_| MetalError::SizeOverflow("3D draw index"))?;
        visit_three_d_tiles(draw_bounds, width, height, tile, |index| {
            let cursor = cursors[index];
            tile_draws[cursor] = draw;
            cursors[index] = cursor + 1;
            Ok(())
        })?;
    }
    Ok((tile_offsets, tile_draws))
}

fn visit_three_d_tiles(
    bounds: Option<[f64; 4]>,
    width: u32,
    height: u32,
    tile: u32,
    mut visit: impl FnMut(usize) -> Result<(), MetalError>,
) -> Result<(), MetalError> {
    let Some(bounds) = bounds else {
        return Ok(());
    };
    if bounds.iter().any(|value| !value.is_finite()) {
        return Err(MetalError::Layout("non-finite 3D draw bounds"));
    }
    let cols = width.div_ceil(tile) as usize;
    let rows = height.div_ceil(tile) as usize;
    let candidate = |minimum: f64, maximum: f64, cells: usize| {
        let scale = f64::from(tile);
        let lo = (minimum / scale).floor() - 1.0;
        let hi = (maximum / scale).ceil() + 1.0;
        let clamp = |value: f64| {
            if value <= 0.0 {
                0
            } else if value >= cells as f64 {
                cells
            } else {
                value as usize
            }
        };
        clamp(lo)..clamp(hi)
    };
    let xs = candidate(bounds[0], bounds[2], cols);
    let ys = candidate(bounds[1], bounds[3], rows);
    for y in ys {
        for x in xs.clone() {
            let x0 = (x as u32).saturating_mul(tile);
            let y0 = (y as u32).saturating_mul(tile);
            let rectangle = [
                x0,
                y0,
                x0.saturating_add(tile).min(width),
                y0.saturating_add(tile).min(height),
            ];
            if bounds[2] >= f64::from(rectangle[0])
                && bounds[3] >= f64::from(rectangle[1])
                && bounds[0] <= f64::from(rectangle[2])
                && bounds[1] <= f64::from(rectangle[3])
            {
                visit(y * cols + x)?;
            }
        }
    }
    Ok(())
}

fn push_segment(
    out: &mut Vec<f32>,
    segment: &Segment,
    config: FrameConfig,
    translate: [f64; 2],
    straight_hint: bool,
) {
    let screen = |point: [f64; 3]| {
        let q = config.map.to_pixel(point[0], point[1]);
        [q[0] + translate[0], q[1] + translate[1]]
    };
    let p0 = screen(segment.p0);
    let p1 = screen(segment.p1);
    let p2 = screen(segment.p2);
    out.extend_from_slice(&[
        p0[0] as f32,
        p0[1] as f32,
        p1[0] as f32,
        p1[1] as f32,
        p2[0] as f32,
        p2[1] as f32,
        segment.s0 as f32,
        segment.s1 as f32,
    ]);

    let total = fmn_geom::arclength::quadratic_arc_length(segment.p0, segment.p1, segment.p2);
    for index in 0..=SEGMENT_ARC_INTERVALS {
        let t = index as f64 / SEGMENT_ARC_INTERVALS as f64;
        let fraction = if total > 0.0 {
            let partial =
                fmn_geom::bezier::partial_quadratic(&[segment.p0, segment.p1, segment.p2], 0.0, t);
            (fmn_geom::arclength::quadratic_arc_length(partial[0], partial[1], partial[2]) / total)
                .clamp(0.0, 1.0)
        } else {
            0.0
        };
        out.push(fraction as f32);
    }
    out.push(
        if crate::stroke::line_approximation_admitted(segment, config.map, straight_hint) {
            1.0
        } else {
            0.0
        },
    );
}

fn scalar_row(slice: &[f32], stride: usize, name: &'static str) -> Result<u32, MetalError> {
    if !slice.len().is_multiple_of(stride) {
        return Err(MetalError::Layout(name));
    }
    u32::try_from(slice.len() / stride).map_err(|_| MetalError::SizeOverflow(name))
}

fn checked_bytes(count: usize, width: usize, name: &'static str) -> Result<usize, MetalError> {
    count
        .checked_mul(width)
        .ok_or(MetalError::SizeOverflow(name))
}

fn nonempty_f32(values: &[f32]) -> &[f32] {
    if values.is_empty() { &[0.0] } else { values }
}

fn nonempty_u32(values: &[u32]) -> &[u32] {
    if values.is_empty() { &[0] } else { values }
}

const fn format_name(format: PixelFormat) -> &'static str {
    match format {
        PixelFormat::Rgba8 => "rgba8",
        PixelFormat::Bgra8 => "bgra8",
        PixelFormat::Rgba16F => "rgba16f",
        PixelFormat::Nv12 => "nv12",
        PixelFormat::P010 => "p010",
    }
}

const fn color_range_name(range: Option<ColorRange>) -> &'static str {
    match range {
        Some(ColorRange::Limited) => "limited",
        Some(ColorRange::Full) => "full",
        None => "none",
    }
}

const fn chroma_siting_name(siting: Option<ChromaSiting>) -> &'static str {
    match siting {
        Some(ChromaSiting::Left) => "left",
        Some(ChromaSiting::Center) => "center",
        None => "none",
    }
}

#[cfg(test)]
mod tests {
    #[cfg(not(target_os = "macos"))]
    use fmn_core::color::LinearRgba;
    use fmn_core::color::Srgb;
    #[cfg(not(target_os = "macos"))]
    use fmn_mobject::Stage;

    use super::*;
    #[cfg(not(target_os = "macos"))]
    use crate::bin::{ScreenMap, Tiling, Viewport};
    use crate::camera::{Camera, CameraConfig};
    use crate::three_d::{SurfaceDraw, SurfaceMesh, SurfaceVertex, ThreeDDraw, TrueDotDraw};

    #[test]
    fn the_shader_mirrors_every_host_stride_and_entry_point() {
        for (name, value) in [
            ("SEGMENT_STRIDE", SEGMENT_STRIDE),
            ("SEGMENT_ARC_INTERVALS", SEGMENT_ARC_INTERVALS),
            ("PIECE_STRIDE", PIECE_STRIDE),
            ("JOIN_STRIDE", JOIN_STRIDE),
            ("STATION_STRIDE", STATION_STRIDE),
            ("DRAW_U32_STRIDE", DRAW_U32_STRIDE),
            ("DRAW_F32_STRIDE", DRAW_F32_STRIDE),
            ("STYLE_STRIDE", STYLE_STRIDE),
            ("THREE_D_VERTEX_STRIDE", THREE_D_VERTEX_STRIDE),
            ("THREE_D_TRIANGLE_STRIDE", THREE_D_TRIANGLE_STRIDE),
            ("THREE_D_DRAW_U32_STRIDE", THREE_D_DRAW_U32_STRIDE),
            ("THREE_D_DRAW_F32_STRIDE", THREE_D_DRAW_F32_STRIDE),
            ("THREE_D_TEXTURE_U32_STRIDE", THREE_D_TEXTURE_U32_STRIDE),
            (
                "TRANSFER_TABLE_WORDS_PER_PLANE",
                TRANSFER_TABLE_WORDS_PER_PLANE,
            ),
        ] {
            assert!(
                KERNEL_SOURCE.contains(&format!("#define {name} {value}")),
                "shader is missing the mirrored `{name}` stride"
            );
        }
        for kernel in [
            RASTER_KERNEL,
            THREE_D_KERNEL,
            RGBA8_KERNEL,
            NV12_KERNEL,
            P010_KERNEL,
        ] {
            assert!(
                KERNEL_SOURCE.contains(&format!("kernel void {kernel}(")),
                "shader is missing `{kernel}`"
            );
        }
    }

    /// The per-command lists `three_d_tile_triangles` built before its two
    /// passes, verbatim: the oracle for the flat arrays and the refusals.
    fn three_d_tile_triangles_by_lists(
        triangles: &[f32],
        draw_u32: &[u32],
        tile_offsets: &[u32],
        tile_draws: &[u32],
        [width, height, tile]: [u32; 3],
        bins: TriangleBins,
    ) -> Result<(Vec<u32>, Vec<u32>), MetalError> {
        let cols = width.div_ceil(tile) as usize;
        let ranges = draw_u32.as_chunks::<THREE_D_DRAW_U32_STRIDE>().0;
        let tile_count = tile_offsets.len().saturating_sub(1);
        // The (tile, command) pairs of each draw, in tile order.
        let mut by_draw: Vec<Vec<(usize, usize)>> = vec![Vec::new(); ranges.len()];
        for tile_index in 0..tile_count {
            let lo = tile_offsets[tile_index] as usize;
            let hi = tile_offsets[tile_index + 1] as usize;
            let commands = tile_draws
                .get(lo..hi)
                .ok_or(MetalError::Layout("3D tile command range"))?;
            for (offset, &draw) in commands.iter().enumerate() {
                by_draw
                    .get_mut(draw as usize)
                    .ok_or(MetalError::Layout("3D tile command names no draw"))?
                    .push((tile_index, lo + offset));
            }
        }
        let mut lists: Vec<Vec<u32>> = vec![Vec::new(); tile_draws.len()];
        let mut command_of_tile = vec![usize::MAX; tile_count];
        for (draw, pairs) in by_draw.iter().enumerate() {
            if pairs.is_empty() {
                continue;
            }
            let [first, count, ..] = ranges[draw];
            let last = first
                .checked_add(count)
                .ok_or(MetalError::SizeOverflow("3D draw triangle range"))?;
            if bins == TriangleBins::Whole {
                for &(_, command) in pairs {
                    lists[command].extend(first..last);
                }
                continue;
            }
            for &(tile_index, command) in pairs {
                command_of_tile[tile_index] = command;
            }
            for triangle in first..last {
                let corner = |index: usize, axis: usize| -> Result<f32, MetalError> {
                    triangles
                        .get(
                            triangle as usize * THREE_D_TRIANGLE_STRIDE
                                + index * THREE_D_VERTEX_STRIDE
                                + axis,
                        )
                        .copied()
                        .filter(|value| value.is_finite())
                        .ok_or(MetalError::Layout("3D triangle corner"))
                };
                let (x_lo, x_hi) = three_d_pixel_span(corner(0, 0)?, corner(1, 0)?, corner(2, 0)?);
                let (y_lo, y_hi) = three_d_pixel_span(corner(0, 1)?, corner(1, 1)?, corner(2, 1)?);
                let (Some((col_lo, col_hi)), Some((row_lo, row_hi))) = (
                    three_d_tile_range(x_lo, x_hi, width, tile),
                    three_d_tile_range(y_lo, y_hi, height, tile),
                ) else {
                    continue;
                };
                for row in row_lo..=row_hi {
                    for col in col_lo..=col_hi {
                        let command = command_of_tile[row * cols + col];
                        if command != usize::MAX {
                            lists[command].push(triangle);
                        }
                    }
                }
            }
            for &(tile_index, _) in pairs {
                command_of_tile[tile_index] = usize::MAX;
            }
        }
        let total = lists.iter().map(Vec::len).sum::<usize>();
        let mut offsets = Vec::with_capacity(lists.len() + 1);
        let mut flat = Vec::with_capacity(total);
        offsets.push(0);
        for list in lists {
            flat.extend_from_slice(&list);
            offsets.push(
                u32::try_from(flat.len())
                    .map_err(|_| MetalError::SizeOverflow("3D tile triangle offset"))?,
            );
        }
        Ok((offsets, flat))
    }

    /// Random 3D jobs that stress per-tile triangle lists: a dense
    /// field-like Gouraud grid, glowing and plain dots, and loose triangles
    /// with corners on and a hair off tile edges (both sides of f32
    /// rounding), partly off screen, sub-pixel and depth-tested.
    /// A camera, its meshes, its dots (centre, radius, glow) and fine tile.
    type TileListJob = (Camera, Vec<SurfaceMesh>, Vec<([f64; 3], f64, bool)>, u32);

    fn tile_list_jobs() -> Vec<TileListJob> {
        let mut state = 0x243F_6A88_85A3_08D3_u64;
        let mut unit = move || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            (state >> 11) as f64 / (1_u64 << 53) as f64
        };
        let mut jobs = Vec::new();
        for (case, &(resolution, samples, fine)) in [
            ((96_u32, 72_u32), 1_u8, 8_u32),
            ((80, 64), 2, 8),
            ((64, 48), 4, 16),
            ((101, 67), 2, 16),
            ((48, 96), 1, 8),
        ]
        .iter()
        .enumerate()
        {
            let camera = Camera::new(CameraConfig {
                resolution,
                samples,
                background: Srgb::from_rgb8(4, 8, 16).to_linear(1.0),
                ..CameraConfig::default()
            })
            .expect("camera");
            let mut meshes = Vec::new();
            // A dense grid over most of the frame, coloured per vertex.
            let (nu, nv) = (24 + case * 3, 18 + case * 2);
            let mut vertices = Vec::new();
            for j in 0..nv {
                for i in 0..nu {
                    let x = -6.5 + 13.0 * i as f64 / (nu - 1) as f64;
                    let y = -3.8 + 7.6 * j as f64 / (nv - 1) as f64;
                    let color = Srgb::from_rgb8(
                        (unit() * 255.0) as u8,
                        (unit() * 255.0) as u8,
                        (unit() * 255.0) as u8,
                    )
                    .to_linear(0.6 + 0.4 * unit());
                    vertices.push(SurfaceVertex::colored([x, y, 0.0], [0.0, 0.0, 1.0], color));
                }
            }
            meshes.push(SurfaceMesh::from_uv_grid(vertices, (nu as u32, nv as u32)).expect("grid"));
            // Loose triangles: random, tiny, huge, partly off screen.
            for _ in 0..40 {
                let scale = [0.02, 0.3, 2.0, 9.0][(unit() * 4.0) as usize % 4];
                let center = [(unit() - 0.5) * 16.0, (unit() - 0.5) * 10.0, unit() - 0.5];
                let corner = |unit: &mut dyn FnMut() -> f64| {
                    [
                        center[0] + (unit() - 0.5) * scale,
                        center[1] + (unit() - 0.5) * scale,
                        center[2],
                    ]
                };
                let color = Srgb::from_rgb8(200, 120, 40).to_linear(0.5 + 0.5 * unit());
                let normal = [0.0, 0.0, 1.0];
                let vertices = vec![
                    SurfaceVertex::colored(corner(&mut unit), normal, color),
                    SurfaceVertex::colored(corner(&mut unit), normal, color),
                    SurfaceVertex::colored(corner(&mut unit), normal, color),
                ];
                meshes.push(SurfaceMesh::new(vertices, vec![0, 1, 2]).expect("triangle"));
            }
            let mut dots = Vec::new();
            for _ in 0..60 {
                dots.push((
                    [(unit() - 0.5) * 15.0, (unit() - 0.5) * 9.0, unit()],
                    0.05 + 0.6 * unit(),
                    unit() < 0.5,
                ));
            }
            jobs.push((camera, meshes, dots, fine));
        }
        jobs
    }

    fn tile_list_draws<'a>(
        meshes: &'a [SurfaceMesh],
        dots: &[([f64; 3], f64, bool)],
        case: usize,
    ) -> Vec<ThreeDDraw<'a>> {
        let white = Srgb::from_rgb8(255, 255, 255).to_linear(0.9);
        let mut draws = Vec::new();
        for (index, mesh) in meshes.iter().enumerate() {
            let mut draw = SurfaceDraw::new(mesh);
            draw.depth_test = (index + case).is_multiple_of(3);
            draws.push(ThreeDDraw::Surface(draw));
        }
        for &(center, radius, glow) in dots {
            draws.push(ThreeDDraw::TrueDot(if glow {
                TrueDotDraw::glow(center, radius, white)
            } else {
                TrueDotDraw::new(center, radius, white)
            }));
        }
        draws
    }

    #[test]
    fn a_three_d_frame_reserves_its_triangle_rows_once_at_their_exact_count() {
        // Grown a triangle at a time, the rows would end with slack (the
        // vector doubles); reserved once, they end exactly full. Every
        // tile-list job, whose rows the other tile-list tests check value by
        // value.
        let mut rows = 0_usize;
        for (case, (camera, meshes, dots, fine)) in tile_list_jobs().into_iter().enumerate() {
            let draws = tile_list_draws(&meshes, &dots, case);
            let job = ThreeDJob::new(
                &camera,
                &draws,
                crate::bin::Tiling {
                    macro_tile: 32,
                    fine_tile: fine,
                },
            )
            .expect("job");
            let expected = job
                .prepared_draws()
                .iter()
                .map(|draw| match &draw.primitive {
                    CompiledPrimitive::Triangles { triangles, .. } => triangles.len(),
                    CompiledPrimitive::Vector(_) => 0,
                })
                .sum::<usize>()
                * THREE_D_TRIANGLE_STRIDE;
            let flat = FlatThreeDFrame::derive(&job).expect("packet");
            assert_eq!(flat.triangles.len(), expected, "case {case}");
            assert_eq!(flat.triangles.capacity(), expected, "case {case}");
            rows += expected / THREE_D_TRIANGLE_STRIDE;
        }
        assert!(rows > 5_000, "{rows} triangle rows");
    }

    #[test]
    fn three_d_tile_triangles_write_the_per_command_lists_flat_arrays() {
        // The two passes write exactly the offsets and triangles the
        // per-command lists concatenated, per tile and whole, on every
        // tile-list job, and refuse a malformed packet with the same error.
        // A packet's arrays, owned: a frame borrows its job's textures.
        struct Packet {
            triangles: Vec<f32>,
            draw_u32: Vec<u32>,
            tile_offsets: Vec<u32>,
            tile_draws: Vec<u32>,
            first_listed: usize,
            dims: [u32; 3],
            bins: TriangleBins,
        }
        let mut listed = 0_usize;
        let mut packets = Vec::new();
        for (case, (camera, meshes, dots, fine)) in tile_list_jobs().into_iter().enumerate() {
            let draws = tile_list_draws(&meshes, &dots, case);
            let job = ThreeDJob::new(
                &camera,
                &draws,
                crate::bin::Tiling {
                    macro_tile: 32,
                    fine_tile: fine,
                },
            )
            .expect("job");
            for bins in [TriangleBins::PerTile, TriangleBins::Whole] {
                let flat = FlatThreeDFrame::derive_with(&job, bins).expect("packet");
                let dims = [flat.params_u32[0], flat.params_u32[1], flat.params_u32[2]];
                let lists = three_d_tile_triangles_by_lists(
                    &flat.triangles,
                    &flat.draw_u32,
                    &flat.tile_offsets,
                    &flat.tile_draws,
                    dims,
                    bins,
                )
                .expect("lists");
                assert_eq!(
                    (&flat.command_triangles, &flat.tile_triangles),
                    (&lists.0, &lists.1),
                    "case {case} {bins:?}"
                );
                listed += flat.tile_triangles.len();
                packets.push(Packet {
                    first_listed: flat.tile_triangles[0] as usize,
                    triangles: flat.triangles,
                    draw_u32: flat.draw_u32,
                    tile_offsets: flat.tile_offsets,
                    tile_draws: flat.tile_draws,
                    dims,
                    bins,
                });
            }
        }
        assert!(listed > 100_000, "{listed} listed triangles");

        let both = |triangles: &[f32],
                    draw_u32: &[u32],
                    offsets: &[u32],
                    draws: &[u32],
                    dims: [u32; 3],
                    bins: TriangleBins| {
            let ours = three_d_tile_triangles(triangles, draw_u32, offsets, draws, dims, bins);
            let lists =
                three_d_tile_triangles_by_lists(triangles, draw_u32, offsets, draws, dims, bins);
            (format!("{ours:?}"), format!("{lists:?}"))
        };
        let mut refused = 0;
        for packet in &packets {
            // A command naming a draw past the last, a reversed tile range
            // and (per tile) a non-finite corner.
            let mut draws = packet.tile_draws.clone();
            let mut offsets = packet.tile_offsets.clone();
            let mut triangles = packet.triangles.clone();
            if let Some(last) = draws.last_mut() {
                *last = (packet.draw_u32.len() / THREE_D_DRAW_U32_STRIDE) as u32;
            }
            let swap = offsets
                .iter()
                .position(|&o| o > 0)
                .expect("a non-empty tile");
            offsets[swap - 1] = offsets[swap] + 1;
            triangles[packet.first_listed * THREE_D_TRIANGLE_STRIDE + 1] = f32::NAN;
            for (triangles, offsets, draws) in [
                (&packet.triangles, &packet.tile_offsets, &draws),
                (&packet.triangles, &offsets, &packet.tile_draws),
                (&triangles, &packet.tile_offsets, &packet.tile_draws),
            ] {
                let (ours, lists) = both(
                    triangles,
                    &packet.draw_u32,
                    offsets,
                    draws,
                    packet.dims,
                    packet.bins,
                );
                assert_eq!(ours, lists, "{:?}", packet.bins);
                if ours.starts_with("Err") {
                    refused += 1;
                }
            }
        }
        // The corner is read per tile only; every other packet refuses.
        assert_eq!(
            refused,
            packets.len() * 2 + packets.len() / 2,
            "{refused} refusals"
        );

        // Two tiles, each binned to one draw of one triangle. Draw 1's
        // triangle also reaches tile 0, where only draw 0 is binned: it is
        // listed in its own command alone, never in draw 0's.
        let mut triangles = vec![0.0_f32; 2 * THREE_D_TRIANGLE_STRIDE];
        for (triangle, corners) in [
            [[1.0, 1.0], [5.0, 1.0], [1.0, 5.0]],
            [[2.0, 1.0], [30.0, 1.0], [2.0, 5.0]],
        ]
        .into_iter()
        .enumerate()
        {
            for (corner, [x, y]) in corners.into_iter().enumerate() {
                let base = triangle * THREE_D_TRIANGLE_STRIDE + corner * THREE_D_VERTEX_STRIDE;
                triangles[base] = x;
                triangles[base + 1] = y;
            }
        }
        let mut draw_u32 = vec![0_u32; 2 * THREE_D_DRAW_U32_STRIDE];
        draw_u32[..2].copy_from_slice(&[0, 1]);
        draw_u32[THREE_D_DRAW_U32_STRIDE..THREE_D_DRAW_U32_STRIDE + 2].copy_from_slice(&[1, 1]);
        let packet = (&triangles, &draw_u32, &[0_u32, 1, 2], &[0_u32, 1]);
        let ours = three_d_tile_triangles(
            packet.0,
            packet.1,
            packet.2,
            packet.3,
            [32, 16, 16],
            TriangleBins::PerTile,
        )
        .expect("two tiles");
        let lists = three_d_tile_triangles_by_lists(
            packet.0,
            packet.1,
            packet.2,
            packet.3,
            [32, 16, 16],
            TriangleBins::PerTile,
        )
        .expect("two tiles");
        assert_eq!(ours, lists);
        assert_eq!(ours, (vec![0, 1, 2], vec![0, 1]));
    }

    #[test]
    fn three_d_tile_lists_drop_only_triangles_outside_every_tile_pixel() {
        // The host replica of three_d_pixel_in_triangle_bounds: a triangle
        // the per-tile lists leave out must fail it at every pixel of the
        // tile, and the kept ones keep the whole list's order.
        let (mut kept, mut dropped) = (0_usize, 0_usize);
        for (case, (camera, meshes, dots, fine)) in tile_list_jobs().into_iter().enumerate() {
            let draws = tile_list_draws(&meshes, &dots, case);
            let job = ThreeDJob::new(
                &camera,
                &draws,
                crate::bin::Tiling {
                    macro_tile: 32,
                    fine_tile: fine,
                },
            )
            .expect("job");
            let tiled = FlatThreeDFrame::derive_with(&job, TriangleBins::PerTile).expect("tiled");
            let whole = FlatThreeDFrame::derive_with(&job, TriangleBins::Whole).expect("whole");
            assert_eq!(tiled.triangles, whole.triangles);
            assert_eq!(tiled.tile_draws, whole.tile_draws);
            let [width, height, tile, cols] = [0, 1, 2, 3].map(|index| tiled.params_u32[index]);
            for tile_index in 0..tiled.tile_offsets.len() - 1 {
                let (tx, ty) = (tile_index as u32 % cols, tile_index as u32 / cols);
                for command in tiled.tile_offsets[tile_index] as usize
                    ..tiled.tile_offsets[tile_index + 1] as usize
                {
                    let list = |flat: &FlatThreeDFrame| {
                        flat.tile_triangles[flat.command_triangles[command] as usize
                            ..flat.command_triangles[command + 1] as usize]
                            .to_vec()
                    };
                    let (short, full) = (list(&tiled), list(&whole));
                    let mut rest = full.iter();
                    assert!(
                        short.iter().all(|t| rest.any(|u| u == t)),
                        "case {case} tile {tile_index}: kept triangles leave the draw's order"
                    );
                    for &triangle in full.iter().filter(|t| !short.contains(t)) {
                        dropped += 1;
                        let base = triangle as usize * THREE_D_TRIANGLE_STRIDE;
                        let corner = |c: usize, axis: usize| {
                            tiled.triangles[base + c * THREE_D_VERTEX_STRIDE + axis]
                        };
                        let xs = [corner(0, 0), corner(1, 0), corner(2, 0)];
                        let ys = [corner(0, 1), corner(1, 1), corner(2, 1)];
                        let (x_lo, x_hi) = (
                            xs[0].min(xs[1].min(xs[2])).floor(),
                            xs[0].max(xs[1].max(xs[2])).ceil(),
                        );
                        let (y_lo, y_hi) = (
                            ys[0].min(ys[1].min(ys[2])).floor(),
                            ys[0].max(ys[1].max(ys[2])).ceil(),
                        );
                        for py in ty * tile..((ty + 1) * tile).min(height) {
                            for px in tx * tile..((tx + 1) * tile).min(width) {
                                let inside = px as f32 >= x_lo
                                    && py as f32 >= y_lo
                                    && (px as f32) < x_hi
                                    && (py as f32) < y_hi;
                                assert!(
                                    !inside,
                                    "case {case}: dropped triangle {triangle} reaches pixel ({px}, {py})"
                                );
                            }
                        }
                    }
                    kept += short.len();
                }
            }
        }
        assert!(
            dropped > 10_000,
            "the lists dropped {dropped} triangle visits"
        );
        assert!(kept > 1_000, "the lists kept {kept} triangle visits");
    }

    #[test]
    fn metal_three_d_tile_lists_render_the_whole_draw_bytes() {
        // The same kernel over per-tile lists and over whole draws (the
        // kernel's behaviour before the lists) must write the same raw
        // RGBA16F bytes, supersampled or not, depth-tested or not.
        if !MetalRenderer::is_available() {
            return;
        }
        let mut renderer = MetalRenderer::new().expect("Metal renderer");
        for (case, (camera, meshes, dots, fine)) in tile_list_jobs().into_iter().enumerate() {
            let draws = tile_list_draws(&meshes, &dots, case);
            let job = ThreeDJob::new(
                &camera,
                &draws,
                crate::bin::Tiling {
                    macro_tile: 32,
                    fine_tile: fine,
                },
            )
            .expect("job");
            let layout = job.layout().expect("layout");
            let mut frames = Vec::new();
            for bins in [TriangleBins::PerTile, TriangleBins::Whole] {
                let flat = FlatThreeDFrame::derive_with(&job, bins).expect("packet");
                renderer
                    .dispatch_three_d(&flat, layout.total_bytes())
                    .expect("dispatch");
                let mut frame = FrameBuffer::new(layout.clone());
                renderer
                    .raw_surface
                    .as_ref()
                    .expect("raw surface")
                    .buffer
                    .read_u8(frame.as_bytes_mut())
                    .expect("read back");
                frames.push(frame);
            }
            let background = frames[1]
                .as_bytes()
                .as_chunks::<8>()
                .0
                .iter()
                .filter(|pixel| **pixel == frames[1].as_bytes()[..8])
                .count();
            assert!(
                background < frames[1].as_bytes().len() / 8,
                "case {case} drew something"
            );
            assert!(
                frames[0].as_bytes() == frames[1].as_bytes(),
                "case {case}: per-tile lists change the frame"
            );
        }
    }

    #[test]
    fn metal_three_d_stays_within_the_cpu_routes_measured_distance() {
        // An oracle outside the kernel. Before the per-tile lists (at
        // 6f786e77) the Metal RGBA8 frames of these jobs differed from the
        // CPU route's by at most 6 levels in at most 5 bytes per job
        // (boundary supersampling decided in f32 against f64). A kernel
        // that skipped or repeated a listed triangle moves whole triangles
        // of these random colours and breaks the bound by far.
        if !MetalRenderer::is_available() {
            return;
        }
        let mut renderer = MetalRenderer::new().expect("Metal renderer");
        for (case, (camera, meshes, dots, fine)) in tile_list_jobs().into_iter().enumerate() {
            let draws = tile_list_draws(&meshes, &dots, case);
            let job = ThreeDJob::new(
                &camera,
                &draws,
                crate::bin::Tiling {
                    macro_tile: 32,
                    fine_tile: fine,
                },
            )
            .expect("job");
            let cpu = render_cpu_three_d_rgba8(&job, 4).expect("CPU frame");
            let (metal, _) = renderer.render_three_d_rgba8(&job).expect("Metal frame");
            assert_eq!(cpu.as_bytes().len(), metal.as_bytes().len());
            let deltas = cpu
                .as_bytes()
                .iter()
                .zip(metal.as_bytes())
                .map(|(a, b)| a.abs_diff(*b))
                .collect::<Vec<_>>();
            let largest = deltas.iter().copied().max().unwrap_or(0);
            let differing = deltas.iter().filter(|delta| **delta > 0).count();
            assert!(
                largest <= 8 && differing <= 32,
                "case {case}: largest byte delta {largest}, {differing} bytes differ"
            );
        }
    }

    /// Noise RGBA8, every tenth texel transparent and the rest partly so.
    fn noise_texture(
        width: u32,
        height: u32,
        seed: u64,
        encoding: crate::texture::TextureEncoding,
    ) -> Texture {
        let mut state = seed;
        let mut next = move || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        };
        let mut rgba = Vec::new();
        for index in 0..width * height {
            for _ in 0..3 {
                rgba.push((next() >> 56) as u8);
            }
            let alpha = 96 + (next() >> 57) as u8;
            rgba.push(if index % 10 == 3 { 0 } else { alpha });
        }
        Texture::from_rgba8(width, height, &rgba, encoding).expect("texture")
    }

    /// A textured quad: `half` extents about `center`, tilted in depth by
    /// `tilt` per unit of x, UV from `uv[0]` to `uv[1]` on both axes, the
    /// left edge's normal and the right edge's.
    fn textured_quad(
        center: [f64; 3],
        half: [f64; 2],
        tilt: f64,
        uv: [f64; 2],
        normals: [[f64; 3]; 2],
        opacity: f64,
    ) -> SurfaceMesh {
        let corner = |sx: f64, sy: f64| {
            [
                center[0] + sx * half[0],
                center[1] + sy * half[1],
                center[2] + sx * tilt,
            ]
        };
        SurfaceMesh::from_uv_grid(
            vec![
                SurfaceVertex::textured(corner(-1.0, 1.0), normals[0], [uv[0], uv[0]], opacity),
                SurfaceVertex::textured(corner(-1.0, -1.0), normals[0], [uv[0], uv[1]], opacity),
                SurfaceVertex::textured(corner(1.0, 1.0), normals[1], [uv[1], uv[0]], opacity),
                SurfaceVertex::textured(corner(1.0, -1.0), normals[1], [uv[1], uv[1]], opacity),
            ],
            (2, 2),
        )
        .expect("quad")
    }

    #[test]
    fn metal_textured_surfaces_stay_within_the_cpu_routes_measured_distance() {
        // An oracle outside the kernel: FrankenManim's CPU route samples the
        // same textures in f64. Textured images (unlit, painter order) and
        // lit surfaces with a dark texture, zero-alpha discard and opacity,
        // under every wrap mode with UV past [0, 1], tilted in depth, beside
        // Gouraud triangles and a glow dot, at one, two and four samples a
        // side.
        use crate::texture::{SamplerPolicy, TextureEncoding, TextureWrap};
        use crate::three_d::SurfaceMaterial;

        if !MetalRenderer::is_available() {
            return;
        }
        let mut renderer = MetalRenderer::new().expect("Metal renderer");
        let noise = noise_texture(7, 5, 0x9E37_79B9_7F4A_7C15, TextureEncoding::Srgb);
        let small = noise_texture(3, 2, 0xD1B5_4A32_D192_ED03, TextureEncoding::Linear);
        let fine = noise_texture(32, 24, 0x8CB9_2BA7_2F3D_8DD7, TextureEncoding::Srgb);
        let dark = noise_texture(5, 4, 0x2545_F491_4F6C_DD1D, TextureEncoding::Srgb);
        let wraps = [
            (TextureWrap::Repeat, TextureWrap::Repeat),
            (TextureWrap::ClampToEdge, TextureWrap::MirroredRepeat),
            (TextureWrap::MirroredRepeat, TextureWrap::ClampToEdge),
        ];
        let facing = [[0.0, 0.0, 1.0]; 2];
        let outward = [[-0.9, 0.0, 0.45], [0.9, 0.0, 0.45]];
        let image = textured_quad(
            [-2.5, 1.0, 0.0],
            [3.0, 2.2],
            0.6,
            [-0.75, 1.75],
            facing,
            0.85,
        );
        let lit = textured_quad(
            [2.5, -0.5, 0.3],
            [3.2, 2.6],
            -0.8,
            [-0.4, 1.3],
            outward,
            1.0,
        );
        let backdrop = textured_quad([0.0, 0.0, -0.5], [6.5, 3.8], 0.0, [0.0, 1.0], facing, 0.6);
        let white = Srgb::from_rgb8(255, 255, 255).to_linear(0.9);
        let colored = SurfaceMesh::new(
            vec![
                SurfaceVertex::colored([-1.0, -3.0, 0.2], [0.0, 0.0, 1.0], white),
                SurfaceVertex::colored([1.5, -2.5, 0.2], [0.0, 0.0, 1.0], white),
                SurfaceVertex::colored([0.0, 2.0, 0.2], [0.0, 0.0, 1.0], white),
            ],
            vec![0, 1, 2],
        )
        .expect("triangle");
        let mut worst = (0_u8, 0_usize);
        for (case, &(resolution, samples, fine_tile)) in [
            ((96_u32, 72_u32), 1_u8, 8_u32),
            ((80, 64), 2, 8),
            ((64, 48), 4, 16),
            ((101, 67), 2, 16),
            ((120, 90), 1, 16),
            ((72, 54), 4, 8),
        ]
        .iter()
        .enumerate()
        {
            let camera = Camera::new(CameraConfig {
                resolution,
                samples,
                background: Srgb::from_rgb8(4, 8, 16).to_linear(1.0),
                ..CameraConfig::default()
            })
            .expect("camera");
            let (wrap_u, wrap_v) = wraps[case % wraps.len()];
            let sampler = SamplerPolicy { wrap_u, wrap_v };
            let tiling = crate::bin::Tiling {
                macro_tile: 32,
                fine_tile,
            };
            let draws_with = |with_dark: bool, sampler: SamplerPolicy| {
                [
                    ThreeDDraw::Surface(SurfaceDraw {
                        material: SurfaceMaterial::Texture(TextureMaterial {
                            light: &fine,
                            dark: None,
                            sampler,
                            discard_transparent: false,
                        }),
                        ..SurfaceDraw::image(&backdrop, &fine)
                    }),
                    ThreeDDraw::Surface(SurfaceDraw::new(&colored)),
                    ThreeDDraw::Surface(SurfaceDraw {
                        material: SurfaceMaterial::Texture(TextureMaterial {
                            light: &noise,
                            dark: None,
                            sampler,
                            discard_transparent: false,
                        }),
                        ..SurfaceDraw::image(&image, &noise)
                    }),
                    ThreeDDraw::Surface(SurfaceDraw {
                        material: SurfaceMaterial::Texture(TextureMaterial {
                            light: &small,
                            dark: with_dark.then_some(&dark),
                            sampler,
                            discard_transparent: true,
                        }),
                        shading: [0.4, 0.3, 0.5],
                        depth_test: true,
                        ..SurfaceDraw::new(&lit)
                    }),
                    ThreeDDraw::TrueDot(TrueDotDraw::glow([0.5, 0.2, 1.0], 0.9, white)),
                ]
            };
            let draws = draws_with(true, sampler);
            let job = ThreeDJob::new(&camera, &draws, tiling).expect("job");
            let cpu = render_cpu_three_d_rgba8(&job, 4).expect("CPU frame");
            let (metal, report) = renderer.render_three_d_rgba8(&job).expect("Metal frame");
            assert_eq!(cpu.as_bytes().len(), metal.as_bytes().len());
            let deltas = cpu
                .as_bytes()
                .iter()
                .zip(metal.as_bytes())
                .map(|(a, b)| a.abs_diff(*b))
                .collect::<Vec<_>>();
            let largest = deltas.iter().copied().max().unwrap_or(0);
            let differing = deltas.iter().filter(|delta| **delta > 0).count();
            println!(
                "case {case} {resolution:?} x{samples}: largest byte delta {largest}, \
                 {differing} of {} bytes differ, {} bytes uploaded",
                deltas.len(),
                report.upload_bytes
            );
            worst = (worst.0.max(largest), worst.1.max(differing));
            assert!(
                largest <= 8 && differing <= 64,
                "case {case}: largest byte delta {largest}, {differing} bytes differ"
            );
            // The job exercises what it claims to: the dark texture and,
            // past [0, 1], the wrap mode change the CPU route's frame.
            let undarkened = draws_with(false, sampler);
            let job = ThreeDJob::new(&camera, &undarkened, tiling).expect("job");
            let without = render_cpu_three_d_rgba8(&job, 4).expect("CPU frame");
            assert_ne!(
                without.as_bytes(),
                cpu.as_bytes(),
                "case {case}: the dark texture shows"
            );
            if case % wraps.len() != 0 {
                let repeated = draws_with(true, SamplerPolicy::default());
                let job = ThreeDJob::new(&camera, &repeated, tiling).expect("job");
                let repeat = render_cpu_three_d_rgba8(&job, 4).expect("CPU frame");
                assert_ne!(
                    repeat.as_bytes(),
                    cpu.as_bytes(),
                    "case {case}: the wrap shows"
                );
            }
        }
        println!(
            "worst: largest byte delta {}, {} bytes differ",
            worst.0, worst.1
        );
    }

    #[test]
    fn a_textured_packet_uploads_each_texture_once_and_names_its_rows() {
        use crate::texture::{SamplerPolicy, TextureEncoding, TextureWrap};
        use crate::three_d::SurfaceMaterial;

        let camera = Camera::new(CameraConfig {
            resolution: (32, 24),
            samples: 1,
            ..CameraConfig::default()
        })
        .expect("camera");
        let shared = noise_texture(4, 3, 1, TextureEncoding::Linear);
        let light = noise_texture(2, 2, 2, TextureEncoding::Linear);
        let dark = noise_texture(3, 1, 3, TextureEncoding::Linear);
        let facing = [[0.0, 0.0, 1.0]; 2];
        let quad = textured_quad([0.0, 0.0, 0.0], [4.0, 3.0], 0.0, [0.0, 1.0], facing, 1.0);
        let white = Srgb::from_rgb8(255, 255, 255).to_linear(1.0);
        let colored = SurfaceMesh::new(
            vec![
                SurfaceVertex::colored([-1.0, -1.0, 0.1], [0.0, 0.0, 1.0], white),
                SurfaceVertex::colored([1.0, -1.0, 0.1], [0.0, 0.0, 1.0], white),
                SurfaceVertex::colored([0.0, 1.0, 0.1], [0.0, 0.0, 1.0], white),
            ],
            vec![0, 1, 2],
        )
        .expect("triangle");
        let draws = [
            ThreeDDraw::Surface(SurfaceDraw::image(&quad, &shared)),
            ThreeDDraw::Surface(SurfaceDraw::image(&quad, &shared)),
            ThreeDDraw::Surface(SurfaceDraw::new(&colored)),
            ThreeDDraw::Surface(SurfaceDraw {
                material: SurfaceMaterial::Texture(TextureMaterial {
                    light: &light,
                    dark: Some(&dark),
                    sampler: SamplerPolicy {
                        wrap_u: TextureWrap::MirroredRepeat,
                        wrap_v: TextureWrap::ClampToEdge,
                    },
                    discard_transparent: true,
                }),
                ..SurfaceDraw::new(&quad)
            }),
        ];
        let job = ThreeDJob::new(
            &camera,
            &draws,
            crate::bin::Tiling {
                macro_tile: 32,
                fine_tile: 8,
            },
        )
        .expect("job");
        let flat = FlatThreeDFrame::derive(&job).expect("packet");

        let texels = flat.texels().expect("texels");
        assert_eq!(texels.len(), (12 + 4 + 3) * 4, "each texture once");
        assert_eq!(&texels[..4], &shared.premultiplied_texels()[0]);
        assert_eq!(&texels[48..52], &light.premultiplied_texels()[0]);
        assert_eq!(&texels[64..68], &dark.premultiplied_texels()[0]);
        assert_eq!(
            flat.texel_key(),
            [&shared, &light, &dark].map(|texture| (
                texture.digest(),
                texture.width(),
                texture.height()
            )),
            "the texels' key names each texture in their order"
        );
        let rows = flat
            .draw_u32
            .as_chunks::<THREE_D_DRAW_U32_STRIDE>()
            .0
            .iter()
            .map(|draw| (draw[2], draw[4]))
            .collect::<Vec<_>>();
        assert_eq!(
            rows,
            [
                (THREE_D_SHADER_TEXTURE, 0),
                (THREE_D_SHADER_TEXTURE, 1),
                (THREE_D_SHADER_GOURAUD, THREE_D_NO_TEXTURE),
                (THREE_D_SHADER_TEXTURE, 2),
            ]
        );
        assert_eq!(
            flat.texture_u32,
            [
                0,
                4,
                3,
                THREE_D_NO_TEXTURE,
                0,
                0,
                0,
                0,
                0,
                4,
                3,
                THREE_D_NO_TEXTURE,
                0,
                0,
                0,
                0,
                12,
                2,
                2,
                16,
                3,
                1,
                2 | 1 << 4,
                1,
            ]
        );
        let scalars = [
            flat.params_u32.len(),
            flat.params_f32.len(),
            flat.triangles.len(),
            flat.draw_u32.len(),
            flat.draw_f32.len(),
            flat.tile_offsets.len(),
            flat.tile_draws.len(),
            flat.command_triangles.len(),
            flat.tile_triangles.len(),
            flat.texture_u32.len(),
            texels.len(),
        ];
        assert_eq!(
            flat.upload_bytes(false).expect("bytes"),
            4 * scalars.iter().sum::<usize>(),
            "the texture tables are uploaded"
        );
        assert_eq!(
            flat.upload_bytes(true).expect("bytes"),
            4 * (scalars.iter().sum::<usize>() - texels.len()),
            "texels a renderer kept are not uploaded again"
        );

        // Rows that would read past the texels, or a textured draw without
        // a row, are refused before any dispatch.
        let mut past = FlatThreeDFrame::derive(&job).expect("packet");
        past.texture_u32[16] = 18;
        assert!(matches!(
            past.validate(),
            Err(MetalError::Layout("3D texture row"))
        ));
        let mut unnamed = FlatThreeDFrame::derive(&job).expect("packet");
        unnamed.draw_u32[THREE_D_DRAW_U32_STRIDE + 4] = THREE_D_NO_TEXTURE;
        assert!(matches!(
            unnamed.validate(),
            Err(MetalError::Layout("3D draw texture row"))
        ));
        let mut wrapped = FlatThreeDFrame::derive(&job).expect("packet");
        wrapped.texture_u32[6] = 3;
        assert!(matches!(
            wrapped.validate(),
            Err(MetalError::Layout("3D texture row"))
        ));
    }

    #[test]
    fn a_frame_sampling_the_same_textures_keeps_their_texels_on_the_device() {
        // FL-033: the renderer keeps the last 3D frame's texels and binds
        // them for a frame that samples the same textures (equal content
        // digests and sizes, in the same order), uploading the rest. The
        // oracle is a fresh renderer, which uploads everything: every frame
        // must equal its frame byte for byte, through the raw and the RGBA8
        // routes.
        use crate::texture::{SamplerPolicy, TextureEncoding};
        use crate::three_d::SurfaceMaterial;

        if !MetalRenderer::is_available() {
            return;
        }
        let fine = noise_texture(32, 24, 0x8CB9_2BA7_2F3D_8DD7, TextureEncoding::Srgb);
        let noise = noise_texture(7, 5, 0x9E37_79B9_7F4A_7C15, TextureEncoding::Srgb);
        let dark = noise_texture(5, 4, 0x2545_F491_4F6C_DD1D, TextureEncoding::Linear);
        // The same size as `noise`, other texels.
        let other = noise_texture(7, 5, 0xD1B5_4A32_D192_ED03, TextureEncoding::Srgb);
        let camera = Camera::new(CameraConfig {
            resolution: (96, 72),
            samples: 2,
            background: Srgb::from_rgb8(4, 8, 16).to_linear(1.0),
            ..CameraConfig::default()
        })
        .expect("camera");
        let tiling = crate::bin::Tiling {
            macro_tile: 32,
            fine_tile: 8,
        };
        let facing = [[0.0, 0.0, 1.0]; 2];
        // Two quads, moved by `shift` (a pan).
        let quads = |shift: f64| {
            (
                textured_quad(
                    [-2.0 + shift, 0.5, 0.0],
                    [3.0, 2.2],
                    0.4,
                    [-0.5, 1.5],
                    facing,
                    0.9,
                ),
                textured_quad(
                    [2.0 + shift, -0.5, 0.3],
                    [2.5, 2.0],
                    -0.3,
                    [0.0, 1.0],
                    facing,
                    1.0,
                ),
            )
        };
        fn draws<'a>(
            back: &'a SurfaceMesh,
            front: &'a SurfaceMesh,
            first: &'a Texture,
            second: &'a Texture,
            dark: &'a Texture,
        ) -> [ThreeDDraw<'a>; 2] {
            [
                ThreeDDraw::Surface(SurfaceDraw::image(back, first)),
                ThreeDDraw::Surface(SurfaceDraw {
                    material: SurfaceMaterial::Texture(TextureMaterial {
                        light: second,
                        dark: Some(dark),
                        sampler: SamplerPolicy::default(),
                        discard_transparent: true,
                    }),
                    shading: [0.4, 0.3, 0.5],
                    ..SurfaceDraw::new(front)
                }),
            ]
        }
        let texel_bytes = |textures: &[&Texture]| {
            16 * textures
                .iter()
                .map(|texture| (texture.width() * texture.height()) as usize)
                .sum::<usize>()
        };
        let mut renderer = MetalRenderer::new().expect("Metal renderer");
        assert_eq!(renderer.resident_texel_bytes(), 0, "nothing before a frame");
        // Each frame through the kept renderer and a fresh one: equal bytes;
        // the upload smaller by exactly the texels when they were kept.
        let mut frame =
            |job: &ThreeDJob<'_>, raw: bool, kept: bool, textures: &[&Texture], step: &str| {
                let mut fresh = MetalRenderer::new().expect("Metal renderer");
                let ((ours, report), (theirs, reference)) = if raw {
                    (
                        renderer.render_three_d_raw(job).expect("Metal frame"),
                        fresh.render_three_d_raw(job).expect("Metal frame"),
                    )
                } else {
                    (
                        renderer.render_three_d_rgba8(job).expect("Metal frame"),
                        fresh.render_three_d_rgba8(job).expect("Metal frame"),
                    )
                };
                assert_eq!(ours.as_bytes(), theirs.as_bytes(), "{step}: the frame");
                let saved = if kept { texel_bytes(textures) } else { 0 };
                assert_eq!(
                    report.upload_bytes + saved,
                    reference.upload_bytes,
                    "{step}: the upload"
                );
                assert_eq!(
                    renderer.resident_texel_bytes(),
                    texel_bytes(textures),
                    "{step}: the kept texels"
                );
                ours.as_bytes().to_vec()
            };

        let (back, front) = quads(0.0);
        let job = ThreeDJob::new(&camera, &draws(&back, &front, &fine, &noise, &dark), tiling)
            .expect("job");
        let first = frame(
            &job,
            false,
            false,
            &[&fine, &noise, &dark],
            "the first frame",
        );

        // A pan: the same textures, kept, through both routes.
        let (back, front) = quads(0.7);
        let job = ThreeDJob::new(&camera, &draws(&back, &front, &fine, &noise, &dark), tiling)
            .expect("job");
        let panned = frame(&job, false, true, &[&fine, &noise, &dark], "a pan");
        assert_ne!(panned, first, "the pan moved the quads");
        frame(&job, true, true, &[&fine, &noise, &dark], "the raw route");

        // Other textures with the same content: kept.
        let (fine2, noise2, dark2) = (fine.clone(), noise.clone(), dark.clone());
        let job = ThreeDJob::new(
            &camera,
            &draws(&back, &front, &fine2, &noise2, &dark2),
            tiling,
        )
        .expect("job");
        frame(&job, false, true, &[&fine, &noise, &dark], "equal textures");

        // One texture of the same size with other texels: uploaded.
        let job = ThreeDJob::new(&camera, &draws(&back, &front, &fine, &other, &dark), tiling)
            .expect("job");
        let changed = frame(
            &job,
            false,
            false,
            &[&fine, &other, &dark],
            "another texture",
        );
        assert_ne!(changed, panned, "the other texture shows");

        // The same textures in another order: uploaded, at their new places.
        let job = ThreeDJob::new(&camera, &draws(&back, &front, &other, &fine, &dark), tiling)
            .expect("job");
        frame(&job, false, false, &[&other, &fine, &dark], "another order");
        frame(
            &job,
            false,
            true,
            &[&other, &fine, &dark],
            "that order again",
        );

        // A frame without textures, then the textures again: uploaded.
        let plain = [ThreeDDraw::TrueDot(TrueDotDraw::glow(
            [0.5, 0.2, 1.0],
            0.9,
            Srgb::from_rgb8(255, 255, 255).to_linear(0.9),
        ))];
        let job = ThreeDJob::new(&camera, &plain, tiling).expect("job");
        frame(&job, false, false, &[], "no textures");
        let job = ThreeDJob::new(&camera, &draws(&back, &front, &other, &fine, &dark), tiling)
            .expect("job");
        frame(
            &job,
            false,
            false,
            &[&other, &fine, &dark],
            "the textures again",
        );
    }

    #[test]
    fn a_bloom_on_the_device_is_the_cpu_routes_bloom_bit_for_bit() {
        // FrankenLife FL-019: the Metal route blooms its raw surface with the
        // CPU route's arithmetic. The oracle is the CPU bloom of the Metal
        // route's own unbloomed raw frame, which keeps the raster's own
        // CPU/Metal distance out of the comparison. Raw and RGBA8 frames are
        // equal byte for byte, the upload is larger by exactly the bloom's
        // inputs, the row sums stay on the device, and a later frame without
        // a bloom is unbloomed.
        use crate::bloom::Bloom;
        use crate::texture::TextureEncoding;

        if !MetalRenderer::is_available() {
            return;
        }
        let texture = noise_texture(32, 24, 0x8CB9_2BA7_2F3D_8DD7, TextureEncoding::Srgb);
        // Not a multiple of the 16-pixel groups.
        let camera = Camera::new(CameraConfig {
            resolution: (100, 70),
            samples: 2,
            background: Srgb::from_rgb8(2, 3, 8).to_linear(1.0),
            ..CameraConfig::default()
        })
        .expect("camera");
        let tiling = crate::bin::Tiling {
            macro_tile: 32,
            fine_tile: 8,
        };
        let quad = textured_quad(
            [-1.5, 0.3, 0.0],
            [2.5, 2.0],
            0.2,
            [-0.5, 1.5],
            [[0.0, 0.0, 1.0]; 2],
            0.9,
        );
        let white = Srgb::from_rgb8(255, 255, 255).to_linear(1.0);
        let draws = [
            ThreeDDraw::Surface(SurfaceDraw::image(&quad, &texture)),
            ThreeDDraw::TrueDot(TrueDotDraw::glow([1.8, 0.4, 0.5], 0.3, white)),
            ThreeDDraw::TrueDot(TrueDotDraw::glow([2.4, -1.1, 0.5], 0.08, white)),
            // Across the right edge, so the edges' clamping shows.
            ThreeDDraw::TrueDot(TrueDotDraw::glow([5.6, 0.0, 0.5], 0.5, white)),
        ];
        let rgba8 = |raw: &FrameBuffer| {
            let layout = FrameLayout::tight(PixelFormat::Rgba8, 100, 70).expect("layout");
            let mut output = FrameBuffer::new(layout);
            fmn_frame::convert::rgba16f_to_rgba8(raw, &mut output).expect("converts");
            output
        };
        let mut renderer = MetalRenderer::new().expect("Metal renderer");
        let plain = ThreeDJob::new(&camera, &draws, tiling).expect("job");
        let (raw, _) = renderer.render_three_d_raw(&plain).expect("Metal frame");
        assert_eq!(renderer.bloom_scratch_bytes(), 0, "nothing before a bloom");
        for (strength, sigma) in [(0.15, 3.0), (0.6, 1.0), (1.0, 7.5)] {
            let bloom = Bloom::new(strength, sigma).expect("bloom");
            let mut expected = raw.clone();
            bloom.apply(&mut expected, 3).expect("blooms");
            let bloomed = ThreeDJob::new(&camera, &draws, tiling)
                .expect("job")
                .with_bloom(bloom.clone());
            let (unbloomed, before) = renderer.render_three_d_raw(&plain).expect("Metal frame");
            assert_eq!(unbloomed.as_bytes(), raw.as_bytes(), "{strength}: no bloom");
            let (ours, report) = renderer.render_three_d_raw(&bloomed).expect("Metal frame");
            assert_eq!(ours.as_bytes(), expected.as_bytes(), "{strength}: raw");
            assert_ne!(ours.as_bytes(), raw.as_bytes(), "{strength}: it blooms");
            assert_eq!(
                report.upload_bytes,
                // One band: its ten sizes; the taps; the mix.
                before.upload_bytes + 40 + 4 * bloom.taps().len() + 8,
                "{strength}: the upload"
            );
            let ring = 70.min(128 + 2 * bloom.radius());
            assert_eq!(renderer.bloom_scratch_bytes(), 100 * ring * 16);
            let (frame, _) = renderer
                .render_three_d_rgba8(&bloomed)
                .expect("Metal frame");
            assert_eq!(
                frame.as_bytes(),
                rgba8(&expected).as_bytes(),
                "{strength}: RGBA8"
            );
            // The CPU route blooms its own raster the same way.
            let mut cpu = plain.render(2).expect("CPU frame");
            bloom.apply(&mut cpu, 2).expect("blooms");
            assert_eq!(
                bloomed.render(2).expect("CPU frame").as_bytes(),
                cpu.as_bytes(),
                "{strength}: CPU"
            );
        }
    }

    #[test]
    fn a_bloom_on_the_device_rounds_every_value_as_the_cpu_does_in_bands_of_any_height() {
        // A raster's few distinct values rarely sit where one rounding more
        // or less shows in half precision. Here every channel of a 256 x 256
        // premultiplied raw surface is its own value, so a mix or row sum the
        // device fused, reordered or summed in other precision would round
        // some of them differently from the CPU bloom, the oracle.
        use crate::bloom::Bloom;
        use fmn_frame::half::f16_from_f32;

        if !MetalRenderer::is_available() {
            return;
        }
        let (width, height) = (256_u32, 256_u32);
        let mut state = 0x9E37_79B9_7F4A_7C15_u64;
        let mut unit = || {
            state = state
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            (state >> 40) as f32 / (1_u64 << 24) as f32
        };
        let mut raw = FrameBuffer::new(
            FrameLayout::tight(PixelFormat::Rgba16F, width, height).expect("layout"),
        );
        for pixel in raw.as_bytes_mut().as_chunks_mut::<8>().0 {
            let alpha = unit();
            let channels = [alpha * unit(), alpha * unit(), alpha * unit(), alpha];
            for (bytes, value) in pixel.as_chunks_mut::<2>().0.iter_mut().zip(channels) {
                *bytes = f16_from_f32(value).to_le_bytes();
            }
        }
        let words: Vec<u32> = raw
            .as_bytes()
            .as_chunks::<4>()
            .0
            .iter()
            .map(|word| u32::from_le_bytes(*word))
            .collect();
        let mut renderer = MetalRenderer::new().expect("Metal renderer");
        // In bands of every height: the default, one row, a few rows across
        // a radius, more than half the frame. The CPU bloom runs in its
        // default bands, so the bits must not depend on the band height.
        let cases = [
            (0.15, 3.0, BLOOM_BAND_ROWS),
            (0.6, 1.0, BLOOM_BAND_ROWS),
            (0.35, 0.7, BLOOM_BAND_ROWS),
            (0.15, 3.0, 1),
            (0.15, 3.0, 7),
            (0.6, 1.0, 100),
        ];
        for (strength, sigma, band) in cases {
            let bloom = Bloom::new(strength, sigma).expect("bloom");
            renderer.bloom_band_rows = band;
            renderer.raw_surface = Some(SurfaceSlot {
                bytes: raw.as_bytes().len(),
                buffer: renderer.gateway.buffer_u32(&words).expect("surface"),
            });
            renderer
                .bloom_raw_surface(width, height, &bloom)
                .expect("device bloom");
            let ring = (band as usize + 2 * bloom.radius()).min(height as usize);
            assert_eq!(renderer.bloom_scratch_bytes(), width as usize * ring * 16);
            let mut ours = raw.clone();
            renderer
                .raw_surface
                .as_ref()
                .expect("surface")
                .buffer
                .read_u8(ours.as_bytes_mut())
                .expect("read back");
            let mut expected = raw.clone();
            bloom.apply(&mut expected, 4).expect("blooms");
            assert_ne!(expected.as_bytes(), raw.as_bytes(), "{strength}: it blooms");
            assert_eq!(ours.as_bytes(), expected.as_bytes(), "{strength} {band}");
        }
    }

    #[test]
    fn three_d_packet_keeps_prepared_triangles_and_stable_painter_bins() {
        let black = Srgb::from_rgb8(0, 0, 0).to_linear(1.0);
        let white = Srgb::from_rgb8(255, 255, 255).to_linear(0.8);
        let camera = Camera::new(CameraConfig {
            resolution: (64, 48),
            samples: 2,
            background: black,
            ..CameraConfig::default()
        })
        .expect("camera");
        let mesh = SurfaceMesh::new(
            vec![
                SurfaceVertex::colored([-2.0, -1.0, 0.0], [0.0, 0.0, 1.0], white),
                SurfaceVertex::colored([2.0, -1.0, 0.0], [0.0, 0.0, 1.0], white),
                SurfaceVertex::colored([0.0, 1.5, 0.0], [0.0, 0.0, 1.0], white),
            ],
            vec![0, 1, 2],
        )
        .expect("surface");
        let draws = [
            ThreeDDraw::Surface(SurfaceDraw::new(&mesh)),
            ThreeDDraw::TrueDot(TrueDotDraw::glow([-0.5, 0.0, 1.0], 0.7, white)),
            ThreeDDraw::TrueDot(TrueDotDraw::new([0.5, 0.0, 1.2], 0.4, white)),
        ];
        let job = ThreeDJob::new(
            &camera,
            &draws,
            crate::bin::Tiling {
                macro_tile: 32,
                fine_tile: 8,
            },
        )
        .expect("prepared 3D job");
        let flat = FlatThreeDFrame::derive(&job).expect("Metal packet");

        assert_eq!(flat.params_u32, [64, 48, 8, 8, 2, 3]);
        assert_eq!(
            flat.triangles.len() / THREE_D_TRIANGLE_STRIDE,
            5,
            "one surface triangle plus two billboard triangles per dot"
        );
        assert_eq!(
            flat.draw_u32
                .as_chunks::<THREE_D_DRAW_U32_STRIDE>()
                .0
                .iter()
                .map(|draw| draw[2])
                .collect::<Vec<_>>(),
            [
                THREE_D_SHADER_GOURAUD,
                THREE_D_SHADER_DOT,
                THREE_D_SHADER_DOT
            ]
        );
        assert!(
            flat.tile_offsets.windows(2).all(|range| {
                flat.tile_draws[range[0] as usize..range[1] as usize]
                    .windows(2)
                    .all(|pair| pair[0] <= pair[1])
            }),
            "stable scatter must never invert painter order"
        );
        assert!(flat.upload_bytes(false).expect("bounded packet") > 0);
    }

    #[test]
    fn backend_identity_excludes_measurement_noise() {
        let base = MetalReport {
            identity: EngineIdentity::metal(),
            device: "fixture".to_owned(),
            unified_memory: true,
            math_mode: "safe",
            kernel_digest: sha256(b"kernel"),
            transfer_table_digest: sha256(b"transfer table"),
            threads_per_threadgroup: 256,
            max_threads_per_threadgroup: 1024,
            thread_execution_width: 32,
            upload_bytes: 10,
            readback_bytes: 20,
            raw_surface_reused: false,
            output_surface_reused: false,
            elapsed: Duration::from_millis(2),
            output_format: PixelFormat::Rgba8,
            color_range: None,
            chroma_siting: None,
        };
        let mut observed = base.clone();
        observed.upload_bytes = 999;
        observed.elapsed = Duration::from_secs(3);
        observed.raw_surface_reused = true;
        assert_eq!(base.backend_digest(), observed.backend_digest());
        observed.device.push_str("-other");
        assert_ne!(base.backend_digest(), observed.backend_digest());
        let mut yuv = base.clone();
        yuv.output_format = PixelFormat::Nv12;
        yuv.color_range = Some(ColorRange::Limited);
        yuv.chroma_siting = Some(ChromaSiting::Left);
        assert_ne!(base.backend_digest(), yuv.backend_digest());
    }

    #[test]
    fn native_preview_validates_dimensions_before_platform_probe() {
        let error =
            NativePreviewRenderer::new(0, 16, "invalid").expect_err("zero width is rejected");
        assert!(matches!(
            error,
            NativePreviewError::Presentation(PresentationError::InvalidDimensions {
                width: 0,
                height: 16
            })
        ));
    }

    #[test]
    fn packed_transfer_table_is_the_authoritative_reel_table() {
        let (words, digest) = build_transfer_table();
        assert_eq!(words.len(), TRANSFER_TABLE_WORDS);
        let tables = fmn_frame::transfer::tables();
        let unpack = |plane: usize, bits: u16| {
            let index = bits as usize;
            let word = words[plane * TRANSFER_TABLE_WORDS_PER_PLANE + index / 4];
            ((word >> ((index % 4) * 8)) & 0xff) as u8
        };
        for bits in [0, 1, 0x3555, 0x3c00, 0x7c00, 0x7e00, u16::MAX] {
            assert_eq!(unpack(0, bits), tables.srgb8_from_f16(bits));
            assert_eq!(unpack(1, bits), tables.linear8_from_f16(bits));
        }
        let rebuilt_bytes = words
            .iter()
            .flat_map(|word| word.to_le_bytes())
            .collect::<Vec<_>>();
        assert_eq!(digest, sha256(&rebuilt_bytes));
    }

    #[test]
    fn preview_fallback_is_reserved_for_backend_failures() {
        assert!(
            MetalError::IncompleteDispatch {
                kernel: RASTER_KERNEL,
                completed: 0,
                expected: 1,
            }
            .permits_preview_fallback()
        );
        assert!(
            MetalError::ThreadgroupTooLarge {
                kernel: RASTER_KERNEL,
                requested: 256,
                maximum: 128,
            }
            .permits_preview_fallback()
        );
        assert!(!MetalError::SizeOverflow("fixture").permits_preview_fallback());
        assert!(
            !MetalError::UnsupportedThreeDPrimitive {
                draw: 0,
                primitive: "fixture",
            }
            .permits_preview_fallback(),
            "frame-local capability gaps must not demote a healthy device"
        );
        assert_eq!(
            NativePreviewError::Presentation(PresentationError::Unavailable).stream_fallback(),
            Some(PreviewFallback::Unavailable)
        );
        assert!(matches!(
            NativePreviewError::Presentation(PresentationError::WrongThread).stream_fallback(),
            Some(PreviewFallback::BackendFailure(_))
        ));
        assert!(
            NativePreviewError::Presentation(PresentationError::InvalidDimensions {
                width: 0,
                height: 1,
            })
            .stream_fallback()
            .is_none(),
            "caller layout errors must not be mislabeled as backend fallback"
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn unsupported_three_d_packet_falls_back_without_demoting_metal() {
        use crate::three_d::VectorDraw;
        use fmn_mobject::{Mobject, RecordBuffer, RecordSchema, Stage};

        if !MetalRenderer::is_available() {
            return;
        }
        let camera = Camera::new(CameraConfig {
            resolution: (16, 16),
            samples: 1,
            background: Srgb::from_rgb8(0, 0, 0).to_linear(1.0),
            ..CameraConfig::default()
        })
        .expect("camera");
        // A closed square of quadratic segments, as the retained IR stores
        // a path: the one 3D primitive the Metal route does not map.
        let corners = [
            [-1.0_f64, -1.0, 0.0],
            [1.0, -1.0, 0.0],
            [1.0, 1.0, 0.0],
            [-1.0, 1.0, 0.0],
        ];
        let mut points = vec![corners[0]];
        for index in 1..=corners.len() {
            let next = corners[index % corners.len()];
            let previous = points[points.len() - 1];
            points.push([
                0.5 * (previous[0] + next[0]),
                0.5 * (previous[1] + next[1]),
                0.5 * (previous[2] + next[2]),
            ]);
            points.push(next);
        }
        let mut buffer =
            RecordBuffer::new(RecordSchema::vmobject(), points.len()).expect("records");
        for (index, point) in points.iter().enumerate() {
            buffer.write(
                index,
                "point",
                &[point[0] as f32, point[1] as f32, point[2] as f32],
            );
            buffer.write(index, "fill_rgba", &[0.2, 0.4, 0.6, 1.0]);
            buffer.write(index, "stroke_rgba", &[0.9, 0.3, 0.1, 1.0]);
            buffer.write(index, "stroke_width", &[2.0]);
            buffer.write(index, "fill_border_width", &[0.0]);
        }
        let mut stage = Stage::new();
        let square = stage.add(Mobject::from_buffer(buffer));
        stage.add_to_scene(square).expect("rooted");
        let mut plan = RenderPlan::new();
        plan.sync(&stage, camera.revision()).expect("vector plan");
        let draws = [ThreeDDraw::Vector(VectorDraw::new(&plan, 0))];
        let job = ThreeDJob::new(&camera, &draws, crate::bin::Tiling::default()).expect("job");
        let expected = render_cpu_three_d_rgba8(&job, 1).expect("CPU frame");
        let mut renderer = PreviewRenderer::new().expect("Metal preview renderer");
        let preview = renderer
            .render_three_d(&job, 1)
            .expect("an unsupported vector uses CPU for this frame");

        assert!(matches!(
            preview.route,
            PreviewRoute::FastCpu(PreviewFallback::Unsupported(_))
        ));
        assert_eq!(preview.frame.as_bytes(), expected.as_bytes());
        assert!(
            matches!(renderer, PreviewRenderer::Metal(_)),
            "a frame-local capability gap must not demote the device"
        );
    }

    #[cfg(not(target_os = "macos"))]
    #[test]
    fn yuv_output_validation_refuses_semantic_mismatches_before_dispatch() {
        let config = FrameConfig::new(
            Viewport {
                width: 16,
                height: 16,
            },
            ScreenMap {
                scale: 1.0,
                origin: [8.0, 8.0],
                y_up: false,
            },
            LinearRgba {
                r: 0.0,
                g: 0.0,
                b: 0.0,
                a: 1.0,
            },
        );
        let p010 = FrameLayout::tight(PixelFormat::P010, 16, 16).expect("valid P010 layout");
        assert!(matches!(
            validate_yuv_output(&p010, config, PixelFormat::P010, ColorRange::Full),
            Err(MetalError::Frame(FrameError::UnsupportedConversion(
                "P010 output is limited-range only"
            )))
        ));

        let rgba = FrameLayout::tight(PixelFormat::Rgba8, 16, 16).expect("valid RGBA layout");
        assert!(matches!(
            validate_yuv_output(&rgba, config, PixelFormat::Nv12, ColorRange::Limited),
            Err(MetalError::Frame(FrameError::FormatMismatch {
                expected: "Nv12 destination",
                got: PixelFormat::Rgba8,
            }))
        ));

        let other_size = FrameLayout::tight(PixelFormat::Nv12, 18, 16).expect("valid NV12 layout");
        assert!(matches!(
            validate_yuv_output(&other_size, config, PixelFormat::Nv12, ColorRange::Limited,),
            Err(MetalError::Frame(FrameError::DimensionMismatch))
        ));
    }

    #[cfg(not(target_os = "macos"))]
    #[test]
    fn unavailable_metal_renders_through_the_truthful_cpu_preview_route() {
        let mut renderer = PreviewRenderer::new().expect("unavailability is a supported state");
        assert!(matches!(
            renderer,
            PreviewRenderer::FastCpu(PreviewFallback::Unavailable)
        ));

        let stage = Stage::new();
        let config = FrameConfig::new(
            Viewport {
                width: 16,
                height: 16,
            },
            ScreenMap {
                scale: 1.0,
                origin: [8.0, 8.0],
                y_up: false,
            },
            LinearRgba {
                r: 0.04,
                g: 0.05,
                b: 0.06,
                a: 1.0,
            },
        );
        let mut plan = RenderPlan::new();
        plan.sync(&stage, 0).expect("valid Metal fixture");
        let mono = MonoTable::build(&plan, config.map).expect("bounded Metal monotone table");
        let binning = Binning::build(&plan, config.viewport, Tiling::default(), config.map)
            .expect("bounded test binning");
        let expected =
            render_cpu_rgba8(&plan, &mono, &binning, config, 1).expect("CPU comparison frame");
        let preview = renderer
            .render(&plan, &mono, &binning, config, 1)
            .expect("CPU fallback renders a preview frame");

        assert_eq!(
            preview.route,
            PreviewRoute::FastCpu(PreviewFallback::Unavailable)
        );
        assert_eq!(preview.identity(), EngineIdentity::fast());
        assert!(preview.metal.is_none());
        assert_eq!(preview.frame.as_bytes(), expected.as_bytes());

        let camera = Camera::new(CameraConfig {
            resolution: (16, 16),
            samples: 1,
            background: config.background,
            ..CameraConfig::default()
        })
        .expect("3D camera");
        let dot = TrueDotDraw::glow(
            [0.0, 0.0, 0.0],
            0.5,
            Srgb::from_rgb8(255, 128, 32).to_linear(0.8),
        );
        let draws = [ThreeDDraw::TrueDot(dot)];
        let job = ThreeDJob::new(&camera, &draws, Tiling::default()).expect("3D job");
        let expected = render_cpu_three_d_rgba8(&job, 1).expect("3D CPU comparison frame");
        let preview = renderer
            .render_three_d(&job, 1)
            .expect("CPU fallback renders a 3D preview frame");
        assert_eq!(
            preview.route,
            PreviewRoute::FastCpu(PreviewFallback::Unavailable)
        );
        assert_eq!(preview.frame.as_bytes(), expected.as_bytes());
    }
}
