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
//! chunks are skipped, never trusted. Chunks must fit inside the declared
//! RIFF container, including their word padding. Extensible PCM/float headers
//! are admitted only after their complete subformat GUID and sample layout
//! have been validated; a matching 16-bit GUID prefix is not a format proof.

/// Typed refusals of the WAV codec.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WavError {
    /// Not a RIFF/WAVE stream.
    NotWav,
    /// The stream ended mid-chunk.
    Truncated,
    /// The RIFF container is malformed or has ambiguous audio chunks.
    BadRiff(&'static str),
    /// No `fmt ` chunk.
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
    /// The sample budget was exceeded.
    TooLarge {
        /// The configured sample budget.
        max_samples: u64,
    },
    /// The decoded sample buffer cannot fit the target's allocation bounds
    /// or available address space, even though its declared budget admits it.
    AllocationFailed {
        /// Number of interleaved f32 samples requested.
        samples: usize,
    },
}

impl std::fmt::Display for WavError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotWav => write!(f, "not a RIFF/WAVE stream"),
            Self::Truncated => write!(f, "wav stream truncated"),
            Self::BadRiff(what) => write!(f, "malformed wav container: {what}"),
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
            Self::TooLarge { max_samples } => {
                write!(f, "wav exceeds the {max_samples}-sample budget")
            }
            Self::AllocationFailed { samples } => {
                write!(f, "cannot allocate {samples} decoded wav samples")
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

/// A decoded (or to-be-encoded) audio buffer: interleaved f32.
#[derive(Debug, Clone, PartialEq)]
pub struct WavAudio {
    /// Channel count.
    pub channels: u16,
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

struct WavSpec {
    channels: u16,
    sample_rate: u32,
    format: SampleFormat,
    bytes_per_sample: usize,
    block_align: usize,
}

fn decode_fmt(body: &[u8]) -> Result<WavSpec, WavError> {
    if body.len() < 16 {
        return Err(WavError::BadFmt("length"));
    }
    let mut tag = read_u16(body, 0)?;
    let channels = read_u16(body, 2)?;
    let sample_rate = read_u32(body, 4)?;
    let byte_rate = read_u32(body, 8)?;
    let block_align = read_u16(body, 12)?;
    let bits = read_u16(body, 14)?;
    if channels == 0 {
        return Err(WavError::BadFmt("zero channels"));
    }
    if sample_rate == 0 {
        return Err(WavError::BadFmt("zero sample rate"));
    }
    if tag == 0xfffe {
        // WAVEFORMATEXTENSIBLE is WAVEFORMATEX (18 bytes), valid bits,
        // channel mask and a full GUID, not just the GUID's format-tag head.
        // https://learn.microsoft.com/en-us/windows/win32/api/mmreg/ns-mmreg-waveformatextensible
        if body.len() < 40 {
            return Err(WavError::BadFmt("extensible length"));
        }
        let extension = usize::from(read_u16(body, 16)?);
        if extension < 22 || extension > body.len() - 18 {
            return Err(WavError::BadFmt("extensible extension size"));
        }
        // DEFINE_WAVEFORMATEX_GUID(tag): tag-0000-0010-8000-00aa00389b71.
        const GUID_TAIL: [u8; 14] = [0, 0, 0, 0, 0x10, 0, 0x80, 0, 0, 0xaa, 0, 0x38, 0x9b, 0x71];
        if body[26..40] != GUID_TAIL {
            return Err(WavError::UnsupportedFormat { format_tag: tag });
        }
        tag = read_u16(body, 24)?;
        let valid_bits = read_u16(body, 18)?;
        if matches!(tag, 1 | 3)
            && (valid_bits == 0 || valid_bits > bits || (tag == 3 && valid_bits != bits))
        {
            return Err(WavError::BadFmt("extensible valid bits"));
        }
        // PCM's valid bits are left-aligned in the sample container. The
        // ordinary signed-container divisors below therefore remain correct
        // for e.g. 20-in-24 and 24-in-32 audio, without a lossy re-scaling.
    }
    let (format, bytes_per_sample) = match (tag, bits) {
        (1, 8) => (SampleFormat::U8, 1),
        (1, 16) => (SampleFormat::S16, 2),
        (1, 24) => (SampleFormat::S24, 3),
        (1, 32) => (SampleFormat::S32, 4),
        (3, 32) => (SampleFormat::F32, 4),
        (1 | 3, other) => return Err(WavError::UnsupportedDepth { bits: other }),
        (other, _) => return Err(WavError::UnsupportedFormat { format_tag: other }),
    };
    let frame_bytes = bytes_per_sample * usize::from(channels);
    if usize::from(block_align) != frame_bytes {
        return Err(WavError::BadFmt("block alignment"));
    }
    if sample_rate.checked_mul(u32::from(block_align)) != Some(byte_rate) {
        return Err(WavError::BadFmt("byte rate"));
    }
    Ok(WavSpec {
        channels,
        sample_rate,
        format,
        bytes_per_sample,
        block_align: frame_bytes,
    })
}

/// Decode a WAV stream.
///
/// Unknown chunks and bytes following the declared RIFF container are ignored.
/// Duplicate format/data chunks are refused rather than silently replacing
/// earlier audio. The format chunk may follow the data chunk.
///
/// # Errors
/// Every refusal in [`WavError`].
pub fn decode_wav(data: &[u8], limits: &WavLimits) -> Result<WavAudio, WavError> {
    if data.len() < 12 || &data[0..4] != b"RIFF" || &data[8..12] != b"WAVE" {
        return Err(WavError::NotWav);
    }
    let size = usize::try_from(read_u32(data, 4)?).map_err(|_| WavError::Truncated)?;
    if size < 4 {
        return Err(WavError::BadRiff("size excludes the WAVE form type"));
    }
    let end = size.checked_add(8).ok_or(WavError::Truncated)?;
    let data = data.get(..end).ok_or(WavError::Truncated)?;
    let mut at = 12usize;
    let mut fmt: Option<&[u8]> = None;
    let mut payload: Option<&[u8]> = None;
    while at < data.len() {
        let body_start = at.checked_add(8).ok_or(WavError::Truncated)?;
        let header = data.get(at..body_start).ok_or(WavError::Truncated)?;
        let name = &header[..4];
        let size = usize::try_from(read_u32(header, 4)?).map_err(|_| WavError::Truncated)?;
        let body_end = body_start.checked_add(size).ok_or(WavError::Truncated)?;
        let body = data.get(body_start..body_end).ok_or(WavError::Truncated)?;
        let next = body_end.checked_add(size & 1).ok_or(WavError::Truncated)?;
        if next > data.len() {
            return Err(WavError::Truncated);
        }
        match name {
            b"fmt " => {
                if fmt.is_some() {
                    return Err(WavError::BadRiff("duplicate fmt chunk"));
                }
                fmt = Some(body);
            }
            b"data" => {
                if payload.is_some() {
                    return Err(WavError::BadRiff("duplicate data chunk"));
                }
                payload = Some(body);
            }
            _ => {}
        }
        // Chunks are word-aligned.
        at = next;
    }
    // Validate the complete container before classifying the codec. An
    // unsupported format may use the explicit transcode capability, but a
    // malformed RIFF file must not escape admission through that fallback.
    let fmt = fmt.ok_or(WavError::MissingFmt)?;
    let payload = payload.ok_or(WavError::MissingData)?;
    let WavSpec {
        channels,
        sample_rate,
        format,
        bytes_per_sample,
        block_align,
    } = decode_fmt(fmt)?;

    if !payload.len().is_multiple_of(block_align) {
        return Err(WavError::BadFmt("incomplete interleaved sample frame"));
    }
    let count = payload.len() / bytes_per_sample;
    if count as u64 > limits.max_samples {
        return Err(WavError::TooLarge {
            max_samples: limits.max_samples,
        });
    }
    // A caller may raise the sample budget above the target's Vec capacity
    // ceiling (notably u8 sources decoded to f32 on wasm32). Refuse that input
    // and allocation failures through the codec's error boundary, not a panic.
    let mut samples = Vec::new();
    samples
        .try_reserve_exact(count)
        .map_err(|_| WavError::AllocationFailed { samples: count })?;
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
    out
}
