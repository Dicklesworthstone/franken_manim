//! Owned WAV/PCM codec (§14.2) — the sound mixer's substrate and the
//! certified audio artifact.
//!
//! Decode accepts the PCM ecosystem (u8, s16, s24, s32, IEEE f32),
//! normalizing samples to interleaved f32 with exact, documented
//! divisors; the original format travels in the spec for provenance.
//! Encode writes s16 (the certified artifact form — defined rounding)
//! or f32 (the lossless intermediate). Compressed WAV variants
//! (ADPCM, µ-law…) are named refusals: the media-transcode capability
//! (the ffmpeg boundary) exists for those.
//!
//! Untrusted-input posture (§16.5): chunk sizes are bounds-checked,
//! the sample budget is enforced before allocation, and unknown
//! chunks are skipped, never trusted. Extensible WAV preserves its speaker
//! mask and validates the complete subtype GUID; channel layout policy is
//! explicit rather than inferred from a multichannel sample count.

/// Typed refusals of the WAV codec.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WavError {
    /// Not a RIFF/WAVE stream.
    NotWav,
    /// The stream ended mid-chunk.
    Truncated,
    /// No `fmt ` chunk before `data`.
    MissingFmt,
    /// No `data` chunk.
    MissingData,
    /// A malformed `fmt ` chunk.
    BadFmt(&'static str),
    /// A compressed or exotic format tag — routed to the transcode
    /// capability, not decoded here.
    UnsupportedFormat {
        /// The refused format tag.
        format_tag: u16,
    },
    /// An unsupported bit depth for the declared format.
    UnsupportedDepth {
        /// The refused bit depth.
        bits: u16,
    },
    /// The declared speakers do not form a supported, unambiguous layout.
    UnsupportedChannelLayout {
        /// The number of interleaved channels.
        channels: u16,
        /// The extensible speaker mask, or unspecified speaker positions.
        channel_mask: Option<u32>,
    },
    /// The sample budget was exceeded.
    TooLarge {
        /// The configured sample budget.
        max_samples: u64,
    },
}

impl std::fmt::Display for WavError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotWav => write!(f, "not a RIFF/WAVE stream"),
            Self::Truncated => write!(f, "wav stream truncated"),
            Self::MissingFmt => write!(f, "wav has no fmt chunk"),
            Self::MissingData => write!(f, "wav has no data chunk"),
            Self::BadFmt(what) => write!(f, "malformed fmt chunk: {what}"),
            Self::UnsupportedFormat { format_tag } => write!(
                f,
                "wav format tag {format_tag} is not PCM/float; use the media-transcode capability"
            ),
            Self::UnsupportedDepth { bits } => {
                write!(f, "unsupported wav bit depth {bits}")
            }
            Self::UnsupportedChannelLayout {
                channels,
                channel_mask,
            } => write!(
                f,
                "unsupported wav channel layout: {channels} channels, mask {channel_mask:?}; multichannel audio requires a supported explicit speaker mask"
            ),
            Self::TooLarge { max_samples } => {
                write!(f, "wav exceeds the {max_samples}-sample budget")
            }
        }
    }
}

impl std::error::Error for WavError {}

/// The source sample format, kept for provenance.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SampleFormat {
    /// Unsigned 8-bit PCM.
    U8,
    /// Signed 16-bit PCM.
    S16,
    /// Signed 24-bit PCM.
    S24,
    /// Signed 32-bit PCM.
    S32,
    /// IEEE 32-bit float.
    F32,
}

/// Supported source speaker layouts in WAVE_FORMAT_EXTENSIBLE mask order.
///
/// Samples occur in increasing speaker-bit order, not a codec-specific
/// surround order. See the [Microsoft speaker-mask specification][spec].
/// Unspecified positions are accepted only for conventional mono/stereo;
/// a channel count alone cannot distinguish, for example, quad from 4.0.
///
/// [spec]: https://learn.microsoft.com/en-us/windows/win32/api/mmreg/ns-mmreg-waveformatextensible
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChannelLayout {
    /// One front-center channel.
    Mono,
    /// Front left and front right.
    Stereo,
    /// Front left, front right, and low-frequency effects (2.1).
    StereoWithLfe,
    /// Front left, front right, and front center (3.0).
    Surround3,
    /// Front left/right and back left/right.
    Quad,
    /// Front left/right, front center, and back center (4.0).
    Surround4,
    /// Front left/right, front center, and back left/right (5.0).
    Surround5,
    /// Front left/right, front center, and side left/right (5.0).
    Surround5Side,
    /// Front left/right/center, LFE, and back left/right (5.1).
    Surround5Point1,
    /// Front left/right/center, LFE, and side left/right (5.1).
    Surround5Point1Side,
    /// Front left/right/center, LFE, back center, and side left/right (6.1).
    Surround6Point1,
    /// Front left/right/center, LFE, back left/right, and side left/right (7.1).
    Surround7Point1,
}

impl ChannelLayout {
    /// The Windows speaker-position mask for this layout.
    #[must_use]
    pub const fn mask(self) -> u32 {
        match self {
            Self::Mono => 0x0004,
            Self::Stereo => 0x0003,
            Self::StereoWithLfe => 0x000b,
            Self::Surround3 => 0x0007,
            Self::Quad => 0x0033,
            Self::Surround4 => 0x0107,
            Self::Surround5 => 0x0037,
            Self::Surround5Side => 0x0607,
            Self::Surround5Point1 => 0x003f,
            Self::Surround5Point1Side => 0x060f,
            Self::Surround6Point1 => 0x070f,
            Self::Surround7Point1 => 0x063f,
        }
    }

    /// The number of channels in this speaker layout.
    #[must_use]
    pub const fn channels(self) -> u16 {
        match self {
            Self::Mono => 1,
            Self::Stereo => 2,
            Self::StereoWithLfe | Self::Surround3 => 3,
            Self::Quad | Self::Surround4 => 4,
            Self::Surround5 | Self::Surround5Side => 5,
            Self::Surround5Point1 | Self::Surround5Point1Side => 6,
            Self::Surround6Point1 => 7,
            Self::Surround7Point1 => 8,
        }
    }

    /// Resolve a declared channel count and optional speaker mask.
    ///
    /// # Errors
    /// Returns [`WavError::UnsupportedChannelLayout`] for unknown positions,
    /// a mask/count mismatch, or multichannel audio with no speaker mask.
    pub fn from_mask(channels: u16, channel_mask: Option<u32>) -> Result<Self, WavError> {
        let layout = match (channels, channel_mask) {
            (1, None | Some(0x0000) | Some(0x0004)) => Some(Self::Mono),
            (2, None | Some(0x0000) | Some(0x0003)) => Some(Self::Stereo),
            (_, Some(mask)) => match mask {
                0x000b => Some(Self::StereoWithLfe),
                0x0007 => Some(Self::Surround3),
                0x0033 => Some(Self::Quad),
                0x0107 => Some(Self::Surround4),
                0x0037 => Some(Self::Surround5),
                0x0607 => Some(Self::Surround5Side),
                0x003f => Some(Self::Surround5Point1),
                0x060f => Some(Self::Surround5Point1Side),
                0x070f => Some(Self::Surround6Point1),
                0x063f => Some(Self::Surround7Point1),
                _ => None,
            },
            _ => None,
        };
        layout.filter(|layout| layout.channels() == channels).ok_or(
            WavError::UnsupportedChannelLayout {
                channels,
                channel_mask,
            },
        )
    }
}

/// A decoded (or to-be-encoded) audio buffer: interleaved f32.
#[derive(Debug, Clone, PartialEq)]
pub struct WavAudio {
    /// Channel count.
    pub channels: u16,
    /// Explicit WAVE_FORMAT_EXTENSIBLE speaker positions.
    ///
    /// `None` includes a zero (unspecified/direct-out) mask. For more than
    /// two channels, consumers must resolve a supported explicit layout
    /// before mapping speakers; they must never guess from the count.
    pub channel_mask: Option<u32>,
    /// Sample rate in Hz.
    pub sample_rate: u32,
    /// The source format (decode) / requested format (encode).
    pub format: SampleFormat,
    /// Interleaved samples, nominal range [-1, 1].
    ///
    /// The exact normalization: u8 → `(v − 128)/128`; s16 → `v/32768`;
    /// s24 → `v/8388608`; s32 → `v/2147483648`; f32 verbatim.
    pub samples: Vec<f32>,
}

impl WavAudio {
    /// Resolve the source speaker layout without changing its samples.
    ///
    /// # Errors
    /// Returns [`WavError::UnsupportedChannelLayout`] for an ambiguous or
    /// unsupported speaker layout.
    pub fn channel_layout(&self) -> Result<ChannelLayout, WavError> {
        ChannelLayout::from_mask(self.channels, self.channel_mask)
    }
}

/// Decode resource budget.
#[derive(Debug, Clone)]
pub struct WavLimits {
    /// Maximum total samples (frames × channels).
    pub max_samples: u64,
}

impl Default for WavLimits {
    /// One hour of 48 kHz stereo.
    fn default() -> Self {
        Self {
            max_samples: 48_000 * 2 * 3600,
        }
    }
}

// The little-endian trailing 12 bytes shared by the registered PCM and
// IEEE-float subtype GUIDs: xxxxxxxx-0000-0010-8000-00aa00389b71.
const WAVE_SUBTYPE_TAIL: [u8; 12] = [
    0x00, 0x00, 0x10, 0x00, 0x80, 0x00, 0x00, 0xaa, 0x00, 0x38, 0x9b, 0x71,
];

struct WaveSpec {
    tag: u16,
    channels: u16,
    rate: u32,
    bits: u16,
    channel_mask: Option<u32>,
}

fn read_u16(data: &[u8], at: usize) -> Result<u16, WavError> {
    data.get(at..at + 2)
        .map(|b| u16::from_le_bytes([b[0], b[1]]))
        .ok_or(WavError::Truncated)
}

fn read_u32(data: &[u8], at: usize) -> Result<u32, WavError> {
    data.get(at..at + 4)
        .map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
        .ok_or(WavError::Truncated)
}

/// Decode a WAV stream.
///
/// # Errors
/// Every refusal in [`WavError`].
pub fn decode_wav(data: &[u8], limits: &WavLimits) -> Result<WavAudio, WavError> {
    if data.len() < 12 || &data[0..4] != b"RIFF" || &data[8..12] != b"WAVE" {
        return Err(WavError::NotWav);
    }
    let mut at = 12usize;
    let mut fmt: Option<WaveSpec> = None;
    let mut payload: Option<&[u8]> = None;
    while at + 8 <= data.len() {
        let name = &data[at..at + 4];
        let size = read_u32(data, at + 4)? as usize;
        let body = data.get(at + 8..at + 8 + size).ok_or(WavError::Truncated)?;
        match name {
            b"fmt " => {
                if size < 16 {
                    return Err(WavError::BadFmt("length"));
                }
                let tag = read_u16(body, 0)?;
                let channels = read_u16(body, 2)?;
                let rate = read_u32(body, 4)?;
                let bits = read_u16(body, 14)?;
                if channels == 0 {
                    return Err(WavError::BadFmt("zero channels"));
                }
                let (tag, channel_mask) = if tag == 0xfffe {
                    if size < 40 {
                        return Err(WavError::BadFmt("truncated extensible format"));
                    }
                    let extension = usize::from(read_u16(body, 16)?);
                    if extension < 22 || extension > size - 18 {
                        return Err(WavError::BadFmt("extensible format size"));
                    }
                    let valid_bits = read_u16(body, 18)?;
                    let mask = read_u32(body, 20)?;
                    // A subtype's first two bytes are insufficient: an
                    // unrelated GUID must not be misread as native PCM.
                    let sub_format = read_u32(body, 24)?;
                    if body[28..40] != WAVE_SUBTYPE_TAIL {
                        return Err(WavError::UnsupportedFormat { format_tag: tag });
                    }
                    let sub_format = u16::try_from(sub_format)
                        .map_err(|_| WavError::UnsupportedFormat { format_tag: tag })?;
                    // The Samples field is a union: compressed formats can
                    // store samples-per-block here, not PCM valid precision.
                    // Preserve their existing typed transcode handoff.
                    if matches!(sub_format, 1 | 3)
                        && (valid_bits == 0 || valid_bits > bits || !bits.is_multiple_of(8))
                    {
                        return Err(WavError::BadFmt("extensible valid bits"));
                    }
                    if sub_format == 3 && valid_bits != bits {
                        return Err(WavError::BadFmt(
                            "IEEE float precision must fill its container",
                        ));
                    }
                    // Integer valid bits are left-aligned in the container,
                    // so the usual container-width divisor remains correct.
                    // Preserve even an unsupported mask: consumer layout
                    // policy is distinct from structural WAV decoding.
                    (sub_format, (mask != 0).then_some(mask))
                } else {
                    (tag, None)
                };
                fmt = Some(WaveSpec {
                    tag,
                    channels,
                    rate,
                    bits,
                    channel_mask,
                });
            }
            b"data" => {
                payload = Some(body);
                // Chunks after data (LIST, etc.) are legal; keep
                // scanning only for fmt if it is somehow late.
            }
            _ => {}
        }
        // Chunks are word-aligned.
        at += 8 + size + (size & 1);
    }
    let WaveSpec {
        tag,
        channels,
        rate: sample_rate,
        bits,
        channel_mask,
    } = fmt.ok_or(WavError::MissingFmt)?;
    let payload = payload.ok_or(WavError::MissingData)?;

    let (format, bytes_per_sample) = match (tag, bits) {
        (1, 8) => (SampleFormat::U8, 1),
        (1, 16) => (SampleFormat::S16, 2),
        (1, 24) => (SampleFormat::S24, 3),
        (1, 32) => (SampleFormat::S32, 4),
        (3, 32) => (SampleFormat::F32, 4),
        (1 | 3, other) => return Err(WavError::UnsupportedDepth { bits: other }),
        (other, _) => return Err(WavError::UnsupportedFormat { format_tag: other }),
    };
    if !payload
        .len()
        .is_multiple_of(bytes_per_sample * usize::from(channels))
    {
        return Err(WavError::BadFmt("incomplete interleaved sample frame"));
    }
    let count = payload.len() / bytes_per_sample;
    if count as u64 > limits.max_samples {
        return Err(WavError::TooLarge {
            max_samples: limits.max_samples,
        });
    }
    let mut samples = Vec::with_capacity(count);
    match format {
        SampleFormat::U8 => {
            for &b in payload {
                samples.push((f32::from(b) - 128.0) / 128.0);
            }
        }
        SampleFormat::S16 => {
            for pair in payload.as_chunks::<2>().0 {
                let v = i16::from_le_bytes([pair[0], pair[1]]);
                samples.push(f32::from(v) / 32768.0);
            }
        }
        SampleFormat::S24 => {
            for triple in payload.as_chunks::<3>().0 {
                let v = i32::from_le_bytes([0, triple[0], triple[1], triple[2]]) >> 8;
                #[allow(clippy::cast_precision_loss)]
                samples.push(v as f32 / 8_388_608.0);
            }
        }
        SampleFormat::S32 => {
            for quad in payload.as_chunks::<4>().0 {
                let v = i32::from_le_bytes([quad[0], quad[1], quad[2], quad[3]]);
                #[allow(clippy::cast_precision_loss)]
                samples.push(v as f32 / 2_147_483_648.0);
            }
        }
        SampleFormat::F32 => {
            for quad in payload.as_chunks::<4>().0 {
                samples.push(f32::from_le_bytes([quad[0], quad[1], quad[2], quad[3]]));
            }
        }
    }
    Ok(WavAudio {
        channels,
        channel_mask,
        sample_rate,
        format,
        samples,
    })
}

/// Quantize one f32 sample to s16: clamp, scale by 32768, round half
/// away from zero — the defined certified-artifact rounding.
fn to_s16(v: f32) -> i16 {
    let scaled = f64::from(v.clamp(-1.0, 1.0)) * 32768.0;
    let rounded = if scaled >= 0.0 {
        (scaled + 0.5).floor()
    } else {
        (scaled - 0.5).ceil()
    };
    rounded.clamp(-32768.0, 32767.0) as i16
}

/// Encode interleaved samples as a WAV file in `format` (s16 or f32
/// only — the artifact forms).
///
/// # Panics
/// Panics if `format` is not `S16` or `F32`, or `channels` is zero —
/// caller bugs, not input conditions.
#[must_use]
#[allow(clippy::cast_possible_truncation)]
pub fn encode_wav(
    channels: u16,
    sample_rate: u32,
    format: SampleFormat,
    samples: &[f32],
) -> Vec<u8> {
    assert!(channels > 0, "zero channels");
    let (tag, bits): (u16, u16) = match format {
        SampleFormat::S16 => (1, 16),
        SampleFormat::F32 => (3, 32),
        other => panic!("encode_wav supports S16/F32, not {other:?}"),
    };
    let bytes_per_sample = usize::from(bits / 8);
    let data_len = samples.len() * bytes_per_sample;
    let block_align = u32::from(channels) * u32::from(bits / 8);
    let byte_rate = sample_rate * block_align;

    let mut out = Vec::with_capacity(44 + data_len);
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(36 + data_len as u32).to_le_bytes());
    out.extend_from_slice(b"WAVE");
    out.extend_from_slice(b"fmt ");
    out.extend_from_slice(&16u32.to_le_bytes());
    out.extend_from_slice(&tag.to_le_bytes());
    out.extend_from_slice(&channels.to_le_bytes());
    out.extend_from_slice(&sample_rate.to_le_bytes());
    out.extend_from_slice(&byte_rate.to_le_bytes());
    out.extend_from_slice(&(block_align as u16).to_le_bytes());
    out.extend_from_slice(&bits.to_le_bytes());
    out.extend_from_slice(b"data");
    out.extend_from_slice(&(data_len as u32).to_le_bytes());
    append_samples(&mut out, format, samples);
    out
}

fn append_samples(out: &mut Vec<u8>, format: SampleFormat, samples: &[f32]) {
    match format {
        SampleFormat::S16 => {
            for &v in samples {
                out.extend_from_slice(&to_s16(v).to_le_bytes());
            }
        }
        _ => {
            for &v in samples {
                out.extend_from_slice(&v.to_le_bytes());
            }
        }
    }
}

/// Encode PCM while retaining an explicit source speaker layout.
///
/// Conventional mono/stereo with no mask retains [`encode_wav`]'s exact
/// 44-byte header. An explicit layout uses a 68-byte extensible header,
/// including valid bits and the full PCM/IEEE-float subtype GUID.
///
/// # Errors
/// Refuses unsupported/ambiguous layouts, malformed sample frames, zero
/// sample rates, non-artifact sample formats, and lengths/rates that
/// cannot be represented by RIFF/WAVE's fields.
pub fn encode_wav_with_layout(audio: &WavAudio, format: SampleFormat) -> Result<Vec<u8>, WavError> {
    audio.channel_layout()?;
    if audio.sample_rate == 0 {
        return Err(WavError::BadFmt("zero sample rate"));
    }
    if !audio
        .samples
        .len()
        .is_multiple_of(usize::from(audio.channels))
    {
        return Err(WavError::BadFmt("incomplete interleaved sample frame"));
    }
    let (tag, bits): (u16, u16) = match format {
        SampleFormat::S16 => (1, 16),
        SampleFormat::F32 => (3, 32),
        _ => return Err(WavError::BadFmt("encoding requires s16 or f32 samples")),
    };
    let bytes_per_sample = u64::from(bits / 8);
    let riff_overhead = if audio.channel_mask.is_some() { 60 } else { 36 };
    let max_samples = (u64::from(u32::MAX) - riff_overhead) / bytes_per_sample;
    let count =
        u64::try_from(audio.samples.len()).map_err(|_| WavError::TooLarge { max_samples })?;
    if count > max_samples {
        return Err(WavError::TooLarge { max_samples });
    }
    let data_len =
        u32::try_from(count * bytes_per_sample).map_err(|_| WavError::TooLarge { max_samples })?;
    let block_align = audio
        .channels
        .checked_mul(bits / 8)
        .ok_or(WavError::BadFmt("block alignment overflow"))?;
    let byte_rate = audio
        .sample_rate
        .checked_mul(u32::from(block_align))
        .ok_or(WavError::BadFmt("byte rate overflow"))?;
    let Some(mask) = audio.channel_mask else {
        return Ok(encode_wav(
            audio.channels,
            audio.sample_rate,
            format,
            &audio.samples,
        ));
    };
    let capacity = usize::try_from(data_len)
        .ok()
        .and_then(|len| len.checked_add(68))
        .ok_or(WavError::TooLarge { max_samples })?;
    let mut out = Vec::with_capacity(capacity);
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(60 + data_len).to_le_bytes());
    out.extend_from_slice(b"WAVE");
    out.extend_from_slice(b"fmt ");
    out.extend_from_slice(&40u32.to_le_bytes());
    out.extend_from_slice(&0xfffeu16.to_le_bytes());
    out.extend_from_slice(&audio.channels.to_le_bytes());
    out.extend_from_slice(&audio.sample_rate.to_le_bytes());
    out.extend_from_slice(&byte_rate.to_le_bytes());
    out.extend_from_slice(&block_align.to_le_bytes());
    out.extend_from_slice(&bits.to_le_bytes());
    out.extend_from_slice(&22u16.to_le_bytes());
    out.extend_from_slice(&bits.to_le_bytes());
    out.extend_from_slice(&mask.to_le_bytes());
    out.extend_from_slice(&u32::from(tag).to_le_bytes());
    out.extend_from_slice(&WAVE_SUBTYPE_TAIL);
    out.extend_from_slice(b"data");
    out.extend_from_slice(&data_len.to_le_bytes());
    append_samples(&mut out, format, &audio.samples);
    Ok(out)
}
