//! Immutable, content-addressed sources for scene sound cues (§14.5).
//!
//! File paths stay unresolved until the composition root supplies filesystem
//! capabilities. Caller-owned PCM is admitted here and frozen as canonical
//! native WAV, so all front doors use Reel's existing decoder and mixer.

use std::borrow::Cow;
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use fmn_codec::{SampleFormat, WavAudio, WavError, WavLimits, decode_wav, encode_wav};
use fmn_hash::{Digest, sha256};

/// Source of a scene sound cue. Memory data is never interpreted as a path.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SoundSource {
    /// The composition root reads this asset once during mix preparation.
    File(PathBuf),
    /// An already validated, immutable canonical PCM WAV snapshot.
    Memory(SoundData),
}

impl SoundSource {
    /// Stable source discriminator for receipts and provenance.
    #[must_use]
    pub const fn kind(&self) -> &'static str {
        match self {
            Self::File(_) => "file",
            Self::Memory(_) => "memory-wav",
        }
    }

    /// The actual asset path, when this cue requires filesystem access.
    #[must_use]
    pub fn file_path(&self) -> Option<&Path> {
        match self {
            Self::File(path) => Some(path),
            Self::Memory(_) => None,
        }
    }

    /// Human-readable source identity. Memory identities are content URIs
    /// for receipts, never synthetic filesystem paths.
    #[must_use]
    pub fn identity(&self) -> Cow<'_, str> {
        match self {
            Self::File(path) => path.to_string_lossy(),
            Self::Memory(data) => Cow::Owned(format!("memory:sha256:{}", data.digest())),
        }
    }
}

/// Admission limits applied before retaining an in-memory sound source.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SoundDataLimits {
    /// Both supplied WAV bytes and the canonical F32 WAV must fit this cap.
    pub max_bytes: usize,
    /// Maximum interleaved samples, before allocation or canonical encoding.
    pub max_samples: usize,
}

impl Default for SoundDataLimits {
    fn default() -> Self {
        Self {
            max_bytes: 64 * 1024 * 1024,
            max_samples: 16 * 1024 * 1024,
        }
    }
}

/// Refusal while admitting caller-supplied PCM, before any scene mutation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SoundDataError {
    /// Invalid metadata, non-finite samples, or an incomplete sample frame.
    Invalid(&'static str),
    /// The declared byte or interleaved-sample budget was exceeded.
    LimitExceeded {
        /// The bounded resource.
        resource: &'static str,
        /// The requested cap.
        limit: usize,
    },
    /// The owned native WAV decoder refused the supplied bytes.
    Decode(WavError),
    /// Memory for canonical samples could not be reserved.
    Allocation,
}

impl fmt::Display for SoundDataError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Invalid(message) => write!(f, "invalid in-memory sound: {message}"),
            Self::LimitExceeded { resource, limit } => {
                write!(f, "in-memory sound exceeds {resource} budget of {limit}")
            }
            Self::Decode(error) => write!(f, "in-memory sound WAV: {error}"),
            Self::Allocation => write!(f, "in-memory sound sample allocation failed"),
        }
    }
}

impl std::error::Error for SoundDataError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Decode(error) => Some(error),
            _ => None,
        }
    }
}

/// Frozen mono/stereo PCM represented by a canonical F32 WAV and its SHA-256.
///
/// Construction copies the caller's samples. Clones share immutable bytes;
/// subsequent changes to the original buffer cannot alter an authored cue.
/// Equivalent PCM, including positive/negative zero and source WAV metadata
/// differences, has one canonical content identity. Finite amplitudes outside
/// [-1, 1] are preserved for the mixer's existing gain and clipping policy.
#[derive(Clone, PartialEq, Eq)]
pub struct SoundData {
    bytes: Arc<[u8]>,
    digest: Digest,
    channels: u16,
    sample_rate: u32,
    sample_frames: usize,
}

impl fmt::Debug for SoundData {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SoundData")
            .field("digest", &self.digest)
            .field("bytes", &self.bytes.len())
            .field("channels", &self.channels)
            .field("sample_rate", &self.sample_rate)
            .field("sample_frames", &self.sample_frames)
            .finish()
    }
}

impl SoundData {
    /// Freeze decoded or synthesized interleaved PCM as canonical native WAV.
    /// The input's `format` is provenance of its original decoder; its f32
    /// samples are preserved without a lossy S16 quantization step.
    ///
    /// # Errors
    /// Unsupported channels, invalid rate/alignment, non-finite samples, a
    /// byte/sample cap, or canonical sample allocation failure.
    pub fn from_audio(audio: &WavAudio, limits: SoundDataLimits) -> Result<Self, SoundDataError> {
        if !matches!(audio.channels, 1 | 2) {
            return Err(SoundDataError::Invalid("channels must be mono or stereo"));
        }
        if audio.sample_rate == 0
            || audio
                .sample_rate
                .checked_mul(u32::from(audio.channels) * 4)
                .is_none()
        {
            return Err(SoundDataError::Invalid(
                "sample rate exceeds native WAV bounds",
            ));
        }
        if !audio
            .samples
            .len()
            .is_multiple_of(usize::from(audio.channels))
        {
            return Err(SoundDataError::Invalid(
                "incomplete interleaved sample frame",
            ));
        }
        if audio.samples.len() > limits.max_samples {
            return Err(SoundDataError::LimitExceeded {
                resource: "samples",
                limit: limits.max_samples,
            });
        }
        let encoded_len = audio
            .samples
            .len()
            .checked_mul(4)
            .and_then(|n| n.checked_add(44));
        if encoded_len.is_none_or(|n| n > limits.max_bytes || n - 8 > u32::MAX as usize) {
            return Err(SoundDataError::LimitExceeded {
                resource: "canonical WAV bytes",
                limit: limits.max_bytes,
            });
        }
        if audio.samples.iter().any(|sample| !sample.is_finite()) {
            return Err(SoundDataError::Invalid("samples must be finite"));
        }
        let mut samples = Vec::new();
        samples
            .try_reserve_exact(audio.samples.len())
            .map_err(|_| SoundDataError::Allocation)?;
        samples.extend(
            audio
                .samples
                .iter()
                .map(|&sample| if sample == 0.0 { 0.0 } else { sample }),
        );
        let bytes: Arc<[u8]> = encode_wav(
            audio.channels,
            audio.sample_rate,
            SampleFormat::F32,
            &samples,
        )
        .into();
        Ok(Self {
            digest: sha256(&bytes),
            bytes,
            channels: audio.channels,
            sample_rate: audio.sample_rate,
            sample_frames: audio.samples.len() / usize::from(audio.channels),
        })
    }

    /// Decode native PCM WAV and freeze its normalized samples canonically.
    /// No subprocess or filesystem capability is used. Both source bytes and
    /// canonical expansion are bounded before the decoder allocates samples.
    ///
    /// # Errors
    /// The native WAV decoder's refusals or [`Self::from_audio`] admission errors.
    pub fn from_wav_bytes(bytes: &[u8], limits: SoundDataLimits) -> Result<Self, SoundDataError> {
        if bytes.len() > limits.max_bytes {
            return Err(SoundDataError::LimitExceeded {
                resource: "source WAV bytes",
                limit: limits.max_bytes,
            });
        }
        let max_samples = limits
            .max_samples
            .min(limits.max_bytes.saturating_sub(44) / 4)
            // A caller may raise its byte cap beyond RIFF's 32-bit length;
            // still reject unrepresentable F32 expansion before decoding.
            .min((u32::MAX as usize - 36) / 4);
        let audio = decode_wav(
            bytes,
            &WavLimits {
                max_samples: max_samples as u64,
            },
        )
        .map_err(SoundDataError::Decode)?;
        Self::from_audio(&audio, limits)
    }

    /// Exact canonical WAV bytes shared by decoding and provenance.
    #[must_use]
    pub const fn bytes(&self) -> &Arc<[u8]> {
        &self.bytes
    }

    /// SHA-256 of [`Self::bytes`].
    #[must_use]
    pub const fn digest(&self) -> Digest {
        self.digest
    }

    /// Number of interleaved channels (one or two).
    #[must_use]
    pub const fn channels(&self) -> u16 {
        self.channels
    }

    /// PCM sample frames per second.
    #[must_use]
    pub const fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    /// PCM frames, each containing [`Self::channels`] samples.
    #[must_use]
    pub const fn sample_frames(&self) -> usize {
        self.sample_frames
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pcm() -> WavAudio {
        WavAudio {
            channels: 2,
            sample_rate: 44_100,
            format: SampleFormat::F32,
            samples: vec![0.0, -0.0, 0.25, -0.5],
        }
    }

    #[test]
    fn canonical_sources_freeze_pcm_and_share_exact_content_identity() {
        let mut original = pcm();
        let frozen = SoundData::from_audio(&original, SoundDataLimits::default()).unwrap();
        original.samples.fill(0.875);
        let decoded = decode_wav(frozen.bytes(), &WavLimits::default()).unwrap();
        assert_eq!(decoded.samples, [0.0, 0.0, 0.25, -0.5]);
        assert_eq!(decoded.samples[1].to_bits(), 0);
        assert_eq!(
            (
                frozen.channels(),
                frozen.sample_rate(),
                frozen.sample_frames()
            ),
            (2, 44_100, 2)
        );
        assert_eq!(frozen.digest(), sha256(frozen.bytes()));
        assert!(Arc::ptr_eq(frozen.bytes(), frozen.clone().bytes()));

        // Source container format and unrelated RIFF metadata do not change
        // the normalized PCM identity, while a sample change does.
        let audio = pcm();
        let mut s16 = encode_wav(
            audio.channels,
            audio.sample_rate,
            SampleFormat::S16,
            &audio.samples,
        );
        s16.extend_from_slice(b"JUNK\x04\0\0\0meta");
        let riff_len = u32::try_from(s16.len() - 8).unwrap();
        s16[4..8].copy_from_slice(&riff_len.to_le_bytes());
        assert_eq!(
            SoundData::from_wav_bytes(&s16, SoundDataLimits::default()).unwrap(),
            frozen
        );
        assert_ne!(
            SoundData::from_audio(&original, SoundDataLimits::default())
                .unwrap()
                .digest(),
            frozen.digest()
        );
        let source = SoundSource::Memory(frozen);
        assert!(source.file_path().is_none());
        assert_eq!(source.kind(), "memory-wav");
        assert!(source.identity().starts_with("memory:sha256:"));
    }

    #[test]
    fn malformed_pcm_and_expansion_budgets_are_refused_before_freezing() {
        for channels in [0, 3, u16::MAX] {
            assert!(matches!(
                SoundData::from_audio(&WavAudio { channels, ..pcm() }, SoundDataLimits::default()),
                Err(SoundDataError::Invalid(_))
            ));
        }
        for sample_rate in [0, u32::MAX] {
            assert!(matches!(
                SoundData::from_audio(
                    &WavAudio {
                        sample_rate,
                        ..pcm()
                    },
                    SoundDataLimits::default()
                ),
                Err(SoundDataError::Invalid(_))
            ));
        }
        for samples in [vec![0.0], vec![f32::NAN, 0.0], vec![0.0, f32::INFINITY]] {
            assert!(matches!(
                SoundData::from_audio(&WavAudio { samples, ..pcm() }, SoundDataLimits::default()),
                Err(SoundDataError::Invalid(_))
            ));
        }
        assert!(matches!(
            SoundData::from_audio(
                &pcm(),
                SoundDataLimits {
                    max_samples: 3,
                    ..SoundDataLimits::default()
                }
            ),
            Err(SoundDataError::LimitExceeded {
                resource: "samples",
                ..
            })
        ));
        assert!(matches!(
            SoundData::from_audio(
                &pcm(),
                SoundDataLimits {
                    max_bytes: 59,
                    ..SoundDataLimits::default()
                }
            ),
            Err(SoundDataError::LimitExceeded {
                resource: "canonical WAV bytes",
                ..
            })
        ));
        let compact = encode_wav(2, 44_100, SampleFormat::S16, &pcm().samples);
        assert_eq!(compact.len(), 52);
        assert!(matches!(
            SoundData::from_wav_bytes(
                &compact,
                SoundDataLimits {
                    max_bytes: 52,
                    max_samples: 4
                }
            ),
            Err(SoundDataError::Decode(WavError::TooLarge { .. }))
        ));
        assert!(matches!(
            SoundData::from_wav_bytes(
                &compact,
                SoundDataLimits {
                    max_bytes: 51,
                    max_samples: 4
                }
            ),
            Err(SoundDataError::LimitExceeded {
                resource: "source WAV bytes",
                ..
            })
        ));
    }
}
