//! The negotiation model of the ffmpeg boundary (§14.3, D-23) — pure
//! data and deterministic argv construction, no processes anywhere.
//!
//! Negotiation replaces the Reference's fixed `rawvideo/rgba → vflip →
//! eq` pipe. fmn-frame renders in output orientation and applies the
//! transfer natively, so **no argv builder in this module can emit
//! `vflip` or an `eq` filter** — there is no code path for either, and
//! the contract suite asserts their absence over every builder.
//!
//! The negotiated dimensions: wire pixel format (RGBA8/BGRA8 for alpha
//! and compatibility, NV12 for ordinary 8-bit video, P010 for 10-bit),
//! color description (primaries, transfer, range), frame rate as an
//! exact rational, container, and encoder. The arithmetic behind NV12:
//! a 3840×2160 RGBA8 frame is 33,177,600 bytes against NV12's
//! 12,441,600 — 2.67× less pipe payload before any copies.

use std::path::Path;

use fmn_frame::{ColorRange, FrameError, FrameLayout, PixelFormat};

mod audio_quality;
pub use audio_quality::AudioQuality;

mod quality;
pub use quality::{EncoderPreset, EncoderTune, VideoQuality};

/// The pixel formats that travel down the pipe to ffmpeg.
///
/// This is deliberately narrower than [`PixelFormat`]: `Rgba16F` is a
/// renderer intermediate, never a wire format.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum WireFormat {
    /// 8-bit RGBA — alpha and compatibility sinks.
    Rgba8,
    /// 8-bit BGRA — compatibility sinks that want it.
    Bgra8,
    /// 4:2:0 8-bit — ordinary video.
    Nv12,
    /// 4:2:0 10-bit — 10-bit/HDR-capable output.
    P010,
}

impl WireFormat {
    /// Resolve the configured wire format for the native, CLI, and Python
    /// front doors. Automatic selection uses NV12 for ordinary video and RGBA
    /// for alpha; ten-bit P010 remains an explicit request. The Reference's
    /// default yuv420p also admits the alpha wire for transparent MOV.
    ///
    /// This selects the bytes sent to the encoder, not an encoder or a
    /// hardware backend. Those still undergo the normal capability probe.
    ///
    /// # Errors
    /// An unsupported setting is refused before any output or process exists.
    pub fn from_config(value: &str, transparent: bool) -> Result<Self, NegotiationError> {
        match value.to_ascii_lowercase().as_str() {
            "auto" | "yuv420p" if transparent => Ok(Self::Rgba8),
            "rgba" | "rgba8" => Ok(Self::Rgba8),
            "bgra" | "bgra8" => Ok(Self::Bgra8),
            // The supported portal hosts represent native-endian ARGB
            // words as BGRA bytes (SceneFileWriter.use_fast_encoding).
            "rgb32" if cfg!(target_endian = "little") => Ok(Self::Bgra8),
            "auto" | "nv12" | "yuv420p" => Ok(Self::Nv12),
            "p010" | "p010le" | "yuv420p10le" => Ok(Self::P010),
            _ => Err(NegotiationError(
                "unsupported file_writer.pixel_format; choose auto, rgba, bgra, nv12/yuv420p, or p010le",
            )),
        }
    }

    /// The ffmpeg `-pix_fmt` name.
    #[must_use]
    pub const fn ffmpeg_pix_fmt(self) -> &'static str {
        match self {
            Self::Rgba8 => "rgba",
            Self::Bgra8 => "bgra",
            Self::Nv12 => "nv12",
            Self::P010 => "p010le",
        }
    }

    /// The fmn-frame format this wire format carries.
    #[must_use]
    pub const fn frame_format(self) -> PixelFormat {
        match self {
            Self::Rgba8 => PixelFormat::Rgba8,
            Self::Bgra8 => PixelFormat::Bgra8,
            Self::Nv12 => PixelFormat::Nv12,
            Self::P010 => PixelFormat::P010,
        }
    }

    /// Bytes of one tightly-packed frame on the wire, validated by the same
    /// layout authority that produces native frame buffers.
    ///
    /// # Errors
    /// Zero dimensions, odd dimensions for 4:2:0 formats, or a layout whose
    /// strides and planes cannot be represented in addressable memory.
    pub fn frame_bytes(self, width: u32, height: u32) -> Result<usize, NegotiationError> {
        FrameLayout::tight(self.frame_format(), width, height)
            .map(|layout| layout.total_bytes())
            .map_err(|error| {
                NegotiationError(match error {
                    FrameError::ZeroDimension => "zero frame dimensions",
                    FrameError::OddDimensions { .. } => {
                        "4:2:0 wire formats require even frame dimensions"
                    }
                    FrameError::TooLarge => "wire frame layout exceeds addressable memory",
                    _ => "invalid wire frame layout",
                })
            })
    }

    /// Whether the wire carries an alpha channel.
    #[must_use]
    pub const fn has_alpha(self) -> bool {
        matches!(self, Self::Rgba8 | Self::Bgra8)
    }
}

/// Color primaries of the already-converted wire samples.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Primaries {
    /// ITU-R BT.709 primaries.
    Bt709,
    /// ITU-R BT.2020 primaries.
    Bt2020,
}

/// Transfer characteristic declared to the sink. fmn-frame applied the
/// transfer once, natively; this flag only *describes* the bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Transfer {
    /// IEC 61966-2-1 (sRGB) — the canonical RGBA path.
    Srgb,
    /// ITU-R BT.709 — the conventional video declaration.
    Bt709,
    /// SMPTE ST 2084 (PQ) — absolute-luminance HDR.
    Pq,
}

/// The complete color description of the wire bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ColorDescription {
    /// Color primaries.
    pub primaries: Primaries,
    /// Transfer characteristic.
    pub transfer: Transfer,
    /// Quantization range.
    pub range: ColorRange,
}

impl ColorDescription {
    /// The description of pixels already transferred with BT.709, limited range.
    /// Do not use this for Lumen output: its native converters apply sRGB.
    /// Use [`Self::for_native_wire`] for those bytes instead.
    #[must_use]
    pub const fn video_bt709() -> Self {
        Self {
            primaries: Primaries::Bt709,
            transfer: Transfer::Bt709,
            range: ColorRange::Limited,
        }
    }

    /// Describe the bytes emitted by fmn-frame's native SDR Lumen converters.
    ///
    /// Both the 8-bit and direct 10-bit YUV paths apply the sRGB transfer
    /// before a BT.709 *matrix*. The matrix does not change the transfer:
    /// declaring those samples BT.709 would change their displayed luminance.
    /// RGB wires carry full-range samples; native NV12/P010 use limited range.
    /// This is metadata only; it neither transforms nor requantizes a frame.
    #[must_use]
    pub const fn for_native_wire(wire: WireFormat) -> Self {
        Self {
            primaries: Primaries::Bt709,
            transfer: Transfer::Srgb,
            range: match wire {
                WireFormat::Rgba8 | WireFormat::Bgra8 => ColorRange::Full,
                WireFormat::Nv12 | WireFormat::P010 => ColorRange::Limited,
            },
        }
    }

    /// The RGBA compatibility description: sRGB transfer, full range.
    #[must_use]
    pub const fn srgb_full() -> Self {
        Self {
            primaries: Primaries::Bt709,
            transfer: Transfer::Srgb,
            range: ColorRange::Full,
        }
    }

    /// Describe native HDR pixels converted to limited-range BT.2020/PQ.
    ///
    /// This is metadata only. The caller must supply P010 samples produced
    /// with the matching primaries, PQ transfer and BT.2020 nonconstant-
    /// luminance matrix; changing the description cannot convert SDR bytes.
    /// It makes no claim about mastering-display or content-light metadata.
    #[must_use]
    pub const fn hdr_pq() -> Self {
        Self {
            primaries: Primaries::Bt2020,
            transfer: Transfer::Pq,
            range: ColorRange::Limited,
        }
    }
}

/// Encoder selection. `Auto` resolves to the documented software
/// default for the color profile and container; hardware encoders are always an explicit,
/// named request (FFMPEG_PROTOCOL.md §5) so acceleration can never be a
/// silent substitution.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EncoderChoice {
    /// The software default (`libx264` for SDR video, `libx265` for PQ HDR,
    /// or `qtrle` for transparent MOV).
    Auto,
    /// A named encoder (software or hardware), validated against the
    /// installed ffmpeg's capabilities before any spawn.
    Named(String),
    /// An explicit encoder and its typed quality policy. Construct through
    /// [`VideoJob::with_quality`] to resolve `Auto` before binding the policy.
    /// Every boundary use revalidates it; direct enum construction cannot
    /// bypass encoder-specific refusals. This carries settings without adding
    /// required fields to existing `VideoJob` struct literals.
    Configured {
        /// The actual ffmpeg encoder name, not an argv fragment.
        name: String,
        /// Opt-in compressed-video controls.
        quality: VideoQuality,
    },
}

/// The negotiated container/mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Container {
    /// MP4 (`+faststart`), yuv output.
    Mp4,
    /// Opaque QuickTime MOV.
    Mov,
    /// Transparent MOV (`qtrle`, argb) — requires an alpha wire format and
    /// the qtrle encoder. Other encoder/profile alpha contracts are not defined.
    MovTransparent,
    /// The retained ffmpeg GIF mode (the native GIF codec needs no
    /// ffmpeg at all; this mode exists for parity).
    Gif,
}

impl Container {
    /// File extension for intermediate artifacts.
    #[must_use]
    pub const fn extension(self) -> &'static str {
        match self {
            Self::Mp4 => "mp4",
            Self::Mov | Self::MovTransparent => "mov",
            Self::Gif => "gif",
        }
    }

    const fn default_encoder(self) -> Option<&'static str> {
        match self {
            Self::Mp4 | Self::Mov => Some("libx264"),
            Self::MovTransparent => Some("qtrle"),
            // GIF is a muxer-level mode; no `-c:v` is emitted.
            Self::Gif => None,
        }
    }
}

/// One negotiated video-encode job.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VideoJob {
    /// Frame width in pixels.
    pub width: u32,
    /// Frame height in pixels.
    pub height: u32,
    /// Exact rational frame rate (numerator, denominator).
    pub fps: (u32, u32),
    /// The wire pixel format.
    pub wire: WireFormat,
    /// The color description of the wire bytes.
    pub color: ColorDescription,
    /// The container/mode.
    pub container: Container,
    /// Encoder selection.
    pub encoder: EncoderChoice,
    /// Constant-rate-factor quality, only meaningful for the software
    /// x264/x265 encoders. [`Self::with_quality`] also accepts it; conflicting
    /// legacy/configured values are refused instead of silently overwritten.
    pub crf: Option<u8>,
}

/// A refused negotiation, named.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NegotiationError(pub &'static str);

impl std::fmt::Display for NegotiationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "refused negotiation: {}", self.0)
    }
}

impl std::error::Error for NegotiationError {}

impl VideoJob {
    /// Select the native BT.2020/PQ HDR wire profile.
    ///
    /// HDR uses limited-range P010 and an opaque MP4 or MOV container. An
    /// automatic encoder becomes the software `libx265`; explicit encoder
    /// choices remain explicit and must advertise 10-bit input at the process
    /// boundary. Apply this before [`Self::with_quality`] when binding quality
    /// controls to the automatic HDR encoder.
    ///
    /// This selects a wire contract, not a color conversion. The producer must
    /// convert its frames to the matching profile before sending them.
    ///
    /// # Errors
    /// A nonopaque container or any other invalid job/quality setting.
    pub fn with_hdr_pq(mut self) -> Result<Self, NegotiationError> {
        self.wire = WireFormat::P010;
        self.color = ColorDescription::hdr_pq();
        if self.encoder == EncoderChoice::Auto {
            self.encoder = EncoderChoice::Named("libx265".to_owned());
        }
        self.resolved_encoder()?;
        Ok(self)
    }

    /// Resolve the encoder name this job will use (`Auto` → the
    /// color profile and container's software default).
    ///
    /// # Errors
    /// [`NegotiationError`] when the job's dimensions contradict each
    /// other (alpha container on an opaque wire, invalid frame geometry or
    /// zero frame rate), a transparent MOV names an encoder outside its qtrle profile,
    /// the PQ profile has incompatible wire/color/container settings, or
    /// quality controls contradict the encoder/container/rate mode.
    pub fn resolved_encoder(&self) -> Result<Option<String>, NegotiationError> {
        self.wire.frame_bytes(self.width, self.height)?;
        if self.fps.0 == 0 || self.fps.1 == 0 {
            return Err(NegotiationError("zero frame rate"));
        }
        if self.color.transfer == Transfer::Pq
            && (self.color.primaries != Primaries::Bt2020
                || self.color.range != ColorRange::Limited
                || self.wire != WireFormat::P010
                || !matches!(self.container, Container::Mp4 | Container::Mov))
        {
            return Err(NegotiationError(
                "PQ HDR requires BT.2020 primaries, limited-range P010, and opaque MP4/MOV",
            ));
        }
        if self.container == Container::MovTransparent && !self.wire.has_alpha() {
            return Err(NegotiationError(
                "transparent MOV requires an alpha wire format (RGBA8/BGRA8)",
            ));
        }
        let encoder = match &self.encoder {
            EncoderChoice::Auto if self.color.transfer == Transfer::Pq => {
                Some("libx265".to_owned())
            }
            EncoderChoice::Auto => self.container.default_encoder().map(str::to_owned),
            EncoderChoice::Named(name) | EncoderChoice::Configured { name, .. } => {
                if self.container == Container::Gif {
                    return Err(NegotiationError(
                        "GIF mode is muxer-level; it takes no encoder",
                    ));
                }
                Some(name.clone())
            }
        };
        // Alpha-bearing input does not prove that an encoder preserves alpha.
        // Keep this invariant in the shared owner so direct Reel and boundary
        // callers receive the same admission as the native and CLI front doors.
        if self.container == Container::MovTransparent && encoder.as_deref() != Some("qtrle") {
            return Err(NegotiationError(
                "transparent video currently requires the qtrle encoder to preserve alpha; select auto or qtrle, or request opaque MOV",
            ));
        }
        self.merged_quality()?
            .validate(encoder.as_deref(), self.container)?;
        Ok(encoder)
    }
}

fn push(argv: &mut Vec<String>, items: &[&str]) {
    argv.extend(items.iter().map(|s| (*s).to_string()));
}

const fn range_name(range: ColorRange) -> &'static str {
    match range {
        ColorRange::Limited => "tv",
        ColorRange::Full => "pc",
    }
}

const fn transfer_name(transfer: Transfer) -> &'static str {
    match transfer {
        Transfer::Srgb => "iec61966-2-1",
        Transfer::Bt709 => "bt709",
        Transfer::Pq => "smpte2084",
    }
}

// Primaries and matrix coefficients are independent: RGB wire bytes have
// the identity matrix regardless of their primary chromaticities.
fn push_color_description(argv: &mut Vec<String>, color: ColorDescription, rgb: bool) {
    let (primaries, matrix) = match color.primaries {
        Primaries::Bt709 => ("bt709", "bt709"),
        Primaries::Bt2020 => ("bt2020", "bt2020nc"),
    };
    push(argv, &["-color_primaries", primaries]);
    push(argv, &["-color_trc", transfer_name(color.transfer)]);
    push(argv, &["-colorspace", if rgb { "rgb" } else { matrix }]);
    push(argv, &["-color_range", range_name(color.range)]);
}

/// The common prefix of every boundary invocation.
fn base_argv() -> Vec<String> {
    vec!["-hide_banner".into(), "-loglevel".into(), "error".into()]
}

/// Build the encode argv: rawvideo frames on stdin → `out`.
///
/// # Errors
/// [`NegotiationError`] per [`VideoJob::resolved_encoder`].
pub fn encode_argv(job: &VideoJob, out: &Path) -> Result<Vec<String>, NegotiationError> {
    build_encode_argv(job, out, None)
}

/// Build encode arguments using the selected encoder's advertised input formats.
///
/// The process boundary supplies the formats from its bounded, fingerprinted
/// `-h encoder=...` probe. Native NV12 and P010 pass through when accepted;
/// otherwise a compatible planar input is selected. P010 requires an
/// advertised 10-bit format, so an 8-bit encoder cannot silently reduce it.
/// `formats` must describe the encoder returned by [`VideoJob::resolved_encoder`].
///
/// # Errors
/// The normal job refusals, or no supported 10-bit input for a P010 job.
pub fn encode_argv_with_formats(
    job: &VideoJob,
    out: &Path,
    formats: &[String],
) -> Result<Vec<String>, NegotiationError> {
    build_encode_argv(job, out, Some(formats))
}

pub(crate) fn encoder_pixel_format(
    job: &VideoJob,
    formats: Option<&[String]>,
) -> Result<Option<&'static str>, NegotiationError> {
    match job.container {
        Container::MovTransparent => Ok(Some("argb")),
        Container::Gif => Ok(None),
        Container::Mp4 | Container::Mov => match (job.wire, formats) {
            (WireFormat::Nv12, Some(formats)) => Ok(Some(
                if formats.iter().any(|format| format == "nv12") {
                    "nv12"
                } else {
                    "yuv420p"
                },
            )),
            (WireFormat::P010, Some(formats)) => [
                "p010le",
                "yuv420p10le",
                "yuv422p10le",
                "yuv444p10le",
            ]
            .into_iter()
            .find(|candidate| formats.iter().any(|format| format.as_str() == *candidate))
            .map(Some)
            .ok_or(NegotiationError(
                "P010 requires an encoder advertising a compatible 10-bit input; choose a 10-bit encoder or NV12",
            )),
            // Pure callers without a capability observation keep the existing
            // conservative argv. Production boundaries always probe YUV jobs.
            (WireFormat::P010, None) => Ok(Some("yuv420p10le")),
            (WireFormat::Nv12, None)
                if job.resolved_encoder()?.as_deref() != Some("libx264") =>
            {
                Ok(None)
            }
            _ => Ok(Some("yuv420p")),
        },
    }
}

fn build_encode_argv(
    job: &VideoJob,
    out: &Path,
    formats: Option<&[String]>,
) -> Result<Vec<String>, NegotiationError> {
    let encoder = job.resolved_encoder()?;
    let quality = job.merged_quality()?;
    let output_pixel_format = encoder_pixel_format(job, formats)?;
    let mut argv = base_argv();
    // Input: tightly-packed frames, output orientation, on stdin.
    push(&mut argv, &["-f", "rawvideo"]);
    push(&mut argv, &["-pix_fmt", job.wire.ffmpeg_pix_fmt()]);
    argv.push("-video_size".into());
    argv.push(format!("{}x{}", job.width, job.height));
    argv.push("-framerate".into());
    argv.push(format!("{}/{}", job.fps.0, job.fps.1));
    // These describe already-converted native pixels, not a requested
    // correction. Without input-side metadata, ffmpeg sees an unknown YUV
    // matrix/range and may insert an unintended color transform when the
    // output is labelled BT.709. Tag both sides of the encode boundary.
    push_color_description(&mut argv, job.color, job.wire.has_alpha());
    push(&mut argv, &["-i", "-"]);

    // Output. Only this pure negotiation layer emits encoder controls.
    if let Some(encoder) = &encoder {
        push(&mut argv, &["-c:v", encoder]);
    }
    if let Some(crf) = quality.crf {
        argv.push("-crf".into());
        argv.push(crf.to_string());
    }
    if let Some(preset) = quality.preset {
        push(&mut argv, &["-preset", preset.as_str()]);
    }
    if let Some(tune) = quality.tune {
        push(&mut argv, &["-tune", tune.as_str()]);
    }
    if let Some(bitrate) = quality.bitrate {
        argv.push("-b:v".into());
        argv.push(bitrate.to_string());
    }
    if let Some(format) = output_pixel_format {
        push(&mut argv, &["-pix_fmt", format]);
    }
    if job.container == Container::Gif {
        push(&mut argv, &["-f", "gif"]);
    }
    if job.container == Container::Mp4 {
        push(&mut argv, &["-movflags", "+faststart"]);
    }
    push_color_description(
        &mut argv,
        job.color,
        matches!(job.container, Container::MovTransparent | Container::Gif),
    );
    argv.push("-y".into());
    argv.push(out.display().to_string());
    Ok(argv)
}

/// Stage 2 of the two-stage audio mux: copy the already-encoded video
/// stream (never re-encode — `-c:v copy` is the contract) and encode
/// the audio to AAC.
#[must_use]
pub fn mux_argv(video: &Path, audio: &Path, out: &Path) -> Vec<String> {
    build_mux_argv(video, audio, out, AudioQuality::default())
}

/// Mux a native soundtrack using an explicit AAC target, retaining stream-copy
/// video and the normal mappings. With no override this is exactly [`mux_argv`].
///
/// # Errors
/// Refuses an invalid AAC target before building process arguments.
pub fn mux_argv_with_quality(
    video: &Path,
    audio: &Path,
    out: &Path,
    quality: AudioQuality,
) -> Result<Vec<String>, NegotiationError> {
    quality.validate()?;
    Ok(build_mux_argv(video, audio, out, quality))
}

fn build_mux_argv(video: &Path, audio: &Path, out: &Path, quality: AudioQuality) -> Vec<String> {
    let mut argv = base_argv();
    push(&mut argv, &["-i"]);
    argv.push(video.display().to_string());
    push(&mut argv, &["-i"]);
    argv.push(audio.display().to_string());
    push(&mut argv, &["-c:v", "copy", "-c:a", "aac"]);
    if let Some(bitrate) = quality.bitrate {
        argv.push("-b:a".into());
        argv.push(bitrate.to_string());
    }
    push(&mut argv, &["-map", "0:v:0", "-map", "1:a:0"]);
    argv.push("-y".into());
    argv.push(out.display().to_string());
    argv
}

/// Concatenate partial movie files (the Reference's insert-file /
/// partial-movie mechanism) with stream copy.
#[must_use]
pub fn concat_argv(list_file: &Path, out: &Path) -> Vec<String> {
    let mut argv = base_argv();
    push(&mut argv, &["-f", "concat", "-safe", "0", "-i"]);
    argv.push(list_file.display().to_string());
    push(&mut argv, &["-map", "0", "-c", "copy"]);
    argv.push("-y".into());
    argv.push(out.display().to_string());
    argv
}

/// Media-transcode capability: decode any audio ffmpeg reads into the
/// engine's native PCM WAV.
#[must_use]
pub fn transcode_audio_argv(input: &Path, out_wav: &Path) -> Vec<String> {
    let mut argv = base_argv();
    push(&mut argv, &["-i"]);
    argv.push(input.display().to_string());
    push(&mut argv, &["-vn", "-acodec", "pcm_s16le", "-f", "wav"]);
    argv.push("-y".into());
    argv.push(out_wav.display().to_string());
    argv
}

/// Media-transcode capability: decode an exotic image into PNG for the
/// native decoder.
#[must_use]
pub fn transcode_image_argv(input: &Path, out_png: &Path) -> Vec<String> {
    let mut argv = base_argv();
    push(&mut argv, &["-i"]);
    argv.push(input.display().to_string());
    push(
        &mut argv,
        &["-frames:v", "1", "-c:v", "png", "-f", "image2"],
    );
    argv.push("-y".into());
    argv.push(out_png.display().to_string());
    argv
}
