//! The standard-only Metal annex executor.
//!
//! This module is the production descendant of G0-8 and G0-8b. It derives a
//! packed, single-typed device layout from the same prepared [`FrameJob`] the
//! CPU engines consume, dispatches only through frankentorch's safe generic
//! gateway, and keeps its output surfaces alive across frames. The semantic
//! front-end, painter-ordered CSR command runs, fill-before-stroke rule, and
//! affine preparation therefore have one authority.
//!
//! The annex never participates in `certified`. Its public preview composition
//! root reports whether Metal actually produced a frame or whether the declared
//! fast-CPU fallback did; CPU bytes are never published under a Metal identity.

use std::fmt;
use std::time::{Duration, Instant};

use fmn_frame::{FrameBuffer, FrameError, FrameLayout, PixelFormat};
use fmn_hash::{Digest, Schema, Writer, sha256};
use fmn_mobject::JointType;
use ft_kernel_metal::Error as GatewayError;
use ft_kernel_metal::compute::{Gateway, Grid, MathMode, Pipeline, SharedBuffer};

use crate::engine::{AaPolicy, Draw, EngineIdentity, FrameConfig, FrameJob, FrameJobError};
use crate::fill::MonoTable;
use crate::plan::RenderPlan;
use crate::{Binning, Segment};

const KERNEL_SOURCE: &str = include_str!("shaders/metal.metal");
const RASTER_KERNEL: &str = "fmn_render_frame";
const RGBA8_KERNEL: &str = "fmn_rgba16f_to_rgba8";
const SEGMENT_ARC_INTERVALS: usize = 16;
const SEGMENT_ARC_VALUES: usize = SEGMENT_ARC_INTERVALS + 1;
const SEGMENT_STRIDE: usize = 8 + SEGMENT_ARC_VALUES + 1;
const PIECE_STRIDE: usize = 6;
const JOIN_STRIDE: usize = 13;
const STATION_STRIDE: usize = 3;
const DRAW_U32_STRIDE: usize = 10;
const DRAW_F32_STRIDE: usize = 8;
const STYLE_STRIDE: usize = 20;
const STATUS_COMPLETE: u32 = 0x464d_4e4d;
const TRANSFER_TILE: usize = 16;

const DRAW_FILL: u32 = 1 << 0;
const DRAW_STROKE: u32 = 1 << 1;
const STROKE_BEHIND: u32 = 1 << 2;
const FLAT_FILL: u32 = 1 << 3;

/// Canonical schema for the annex-specific half of C7.
pub const METAL_BACKEND_SCHEMA: Schema = Schema::new(*b"FMNM", 1, 0, 0);

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

/// Maximum encoded-code error for the GPU RGBA16F-to-RGBA8 transfer.
///
/// The MSL path uses safe math and is compared against Reel's canonical table
/// conversion from the same raw surface. One 8-bit code is the admitted
/// standard-preview rounding difference.
pub const METAL_RGBA8_TRANSFER_V1_MAX_CODE_ERROR: u8 = 1;

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
            | Self::Layout(_) => None,
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
    /// Fine-tile threads used by the raster pipeline.
    pub threads_per_threadgroup: usize,
    /// Raster pipeline occupancy ceiling.
    pub max_threads_per_threadgroup: usize,
    /// Raster pipeline SIMD execution width.
    pub thread_execution_width: usize,
    /// Bytes materialized into new input buffers for this frame.
    pub upload_bytes: usize,
    /// Bytes copied to the host for the requested output.
    pub readback_bytes: usize,
    /// Whether the renderer reused its lifetime-held raw surface.
    pub raw_surface_reused: bool,
    /// Whether a converted output surface was reused.
    pub output_surface_reused: bool,
    /// Host-observed prepare/upload/dispatch/readback wall time.
    pub elapsed: Duration,
    /// Format copied back to the host.
    pub output_format: PixelFormat,
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

/// Why Studio preview used the CPU route.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PreviewFallback {
    /// This target or machine has no usable Metal device.
    Unavailable,
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
    Metal(MetalRenderer),
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
            Ok(renderer) => Ok(Self::Metal(renderer)),
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
        let reason = match self {
            Self::Metal(renderer) => match renderer.render_rgba8(plan, mono, binning, config) {
                Ok((frame, report)) => {
                    return Ok(PreviewFrame {
                        frame,
                        route: PreviewRoute::Metal,
                        metal: Some(report),
                    });
                }
                Err(error) if error.permits_preview_fallback() => {
                    PreviewFallback::BackendFailure(error.to_string())
                }
                Err(error) => return Err(error),
            },
            Self::FastCpu(reason) => reason.clone(),
        };
        *self = Self::FastCpu(reason.clone());
        let frame = render_cpu_rgba8(plan, mono, binning, config, cpu_threads)?;
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
    rgba8: Pipeline,
    raw_surface: Option<SurfaceSlot>,
    output_surface: Option<SurfaceSlot>,
    device: String,
    unified_memory: bool,
    kernel_digest: Digest,
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
                &self.output_surface.as_ref().map(|s| s.bytes),
            )
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
        let rgba8 = library.pipeline(RGBA8_KERNEL)?;
        Ok(Self {
            gateway,
            raster,
            rgba8,
            raw_surface: None,
            output_surface: None,
            device: gateway.device_name(),
            unified_memory: gateway.has_unified_memory(),
            kernel_digest: sha256(KERNEL_SOURCE.as_bytes()),
        })
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
            &flat,
            upload_bytes,
            frame.as_bytes().len(),
            raw_reused,
            false,
            started.elapsed(),
            PixelFormat::Rgba16F,
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
        let job = FrameJob::for_metal(plan, mono, binning, config)?;
        let flat = FlatFrame::derive(&job)?;
        let raw_layout = config.layout()?;
        let (raw_reused, upload_bytes) = self.dispatch_raster(&flat, raw_layout.total_bytes())?;

        let output_layout = FrameLayout::tight(
            PixelFormat::Rgba8,
            config.viewport.width,
            config.viewport.height,
        )?;
        let output_reused = ensure_surface(
            self.gateway,
            &mut self.output_surface,
            output_layout.total_bytes(),
        )?;
        let transfer_params = [
            config.viewport.width,
            config.viewport.height,
            u32::try_from(output_layout.stride(0))
                .map_err(|_| MetalError::SizeOverflow("RGBA8 stride"))?,
        ];
        let params = self.gateway.buffer_u32(&transfer_params)?;
        let groups_x = (config.viewport.width as usize).div_ceil(TRANSFER_TILE);
        let groups_y = (config.viewport.height as usize).div_ceil(TRANSFER_TILE);
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

        let mut frame = FrameBuffer::new(output_layout);
        self.output_surface
            .as_ref()
            .ok_or(MetalError::Layout("RGBA8 surface disappeared"))?
            .buffer
            .read_u8(frame.as_bytes_mut())?;
        let report = self.report(
            &flat,
            upload_bytes
                .checked_add(std::mem::size_of_val(&transfer_params))
                .ok_or(MetalError::SizeOverflow("frame upload count"))?,
            frame.as_bytes().len(),
            raw_reused,
            output_reused,
            started.elapsed(),
            PixelFormat::Rgba8,
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

    #[allow(clippy::too_many_arguments)]
    fn report(
        &self,
        flat: &FlatFrame,
        upload_bytes: usize,
        readback_bytes: usize,
        raw_surface_reused: bool,
        output_surface_reused: bool,
        elapsed: Duration,
        output_format: PixelFormat,
    ) -> MetalReport {
        let threads = flat.tile().saturating_mul(flat.tile());
        MetalReport {
            identity: EngineIdentity::metal(),
            device: self.device.clone(),
            unified_memory: self.unified_memory,
            math_mode: "safe",
            kernel_digest: self.kernel_digest,
            threads_per_threadgroup: threads,
            max_threads_per_threadgroup: self.raster.max_threads_per_threadgroup(),
            thread_execution_width: self.raster.thread_execution_width(),
            upload_bytes,
            readback_bytes,
            raw_surface_reused,
            output_surface_reused,
            elapsed,
            output_format,
        }
    }
}

struct SurfaceSlot {
    bytes: usize,
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
        for join in &draw.joins {
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
        if let Some(field) = &draw.field {
            let (points, params) = field.stations();
            if points.len() != params.len() {
                return Err(MetalError::Layout("gradient stations are not parallel"));
            }
            for (point, &param) in points.iter().zip(params) {
                self.stations.extend_from_slice(&[
                    (point[0] + draw.translate[0]) as f32,
                    (point[1] + draw.translate[1]) as f32,
                    param as f32,
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
        let stroke_slab = draw
            .stroke
            .as_ref()
            .map_or([0.0; 4], |stroke| stroke.slab());
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

fn push_segment(
    out: &mut Vec<f32>,
    segment: &Segment,
    config: FrameConfig,
    translate: [f64; 2],
    straight_hint: bool,
) {
    let screen = |point: [f64; 3]| {
        [
            config.map.origin[0] + point[0] * config.map.scale + translate[0],
            config.map.origin[1] + point[1] * config.map.scale + translate[1],
        ]
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

#[cfg(test)]
mod tests {
    #[cfg(not(target_os = "macos"))]
    use fmn_core::color::LinearRgba;
    #[cfg(not(target_os = "macos"))]
    use fmn_mobject::Stage;

    use super::*;
    #[cfg(not(target_os = "macos"))]
    use crate::bin::{ScreenMap, Tiling, Viewport};

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
        ] {
            assert!(
                KERNEL_SOURCE.contains(&format!("#define {name} {value}")),
                "shader is missing the mirrored `{name}` stride"
            );
        }
        for kernel in [RASTER_KERNEL, RGBA8_KERNEL] {
            assert!(
                KERNEL_SOURCE.contains(&format!("kernel void {kernel}(")),
                "shader is missing `{kernel}`"
            );
        }
    }

    #[test]
    fn backend_identity_excludes_measurement_noise() {
        let base = MetalReport {
            identity: EngineIdentity::metal(),
            device: "fixture".to_owned(),
            unified_memory: true,
            math_mode: "safe",
            kernel_digest: sha256(b"kernel"),
            threads_per_threadgroup: 256,
            max_threads_per_threadgroup: 1024,
            thread_execution_width: 32,
            upload_bytes: 10,
            readback_bytes: 20,
            raw_surface_reused: false,
            output_surface_reused: false,
            elapsed: Duration::from_millis(2),
            output_format: PixelFormat::Rgba8,
        };
        let mut observed = base.clone();
        observed.upload_bytes = 999;
        observed.elapsed = Duration::from_secs(3);
        observed.raw_surface_reused = true;
        assert_eq!(base.backend_digest(), observed.backend_digest());
        observed.device.push_str("-other");
        assert_ne!(base.backend_digest(), observed.backend_digest());
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
            },
            LinearRgba {
                r: 0.04,
                g: 0.05,
                b: 0.06,
                a: 1.0,
            },
        );
        let mut plan = RenderPlan::new();
        plan.sync(&stage, 0);
        let mono = MonoTable::build(&plan, config.map);
        let binning = Binning::build(&plan, config.viewport, Tiling::default(), config.map);
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
    }
}
