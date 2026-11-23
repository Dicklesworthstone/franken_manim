//! Immutable, content-addressed sources for scene sound cues (§14.5).
//!
//! File paths stay unresolved until the composition root supplies filesystem
//! capabilities. Caller-owned PCM is admitted here and frozen as canonical
//! native WAV, so all front doors use Reel's existing decoder and mixer.

use std::borrow::Cow;
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use fmn_codec::{SampleFormat, WavAudio, WavError, WavLimits, decode_wav, encode_wav_with_layout};
use fmn_hash::{Digest, sha256};

/// Source of a scene sound cue. Memory data is never interpreted as a path.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SoundSource {
    /// The composition root reads this asset once during mix preparation.
    File(PathBuf),
    /// An already validated, immutable canonical PCM WAV snapshot.
    Memory(SoundData),
}

/// A preparation-cache key that keeps file names and immutable content separate.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum SoundSourceKey {
    /// An authored path, read once by the composition root.
    File(PathBuf),
    /// The canonical WAV digest of a frozen memory asset.
    Memory(Digest),
}

impl fmt::Display for SoundSourceKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::File(path) => write!(f, "{}", path.display()),
            Self::Memory(digest) => write!(f, "memory:sha256:{digest}"),
        }
    }
}

impl SoundSource {
    /// Identity used to share source reads and decodes within one preparation.
    #[must_use]
    pub fn key(&self) -> SoundSourceKey {
        match self {
            Self::File(path) => SoundSourceKey::File(path.clone()),
            Self::Memory(data) => SoundSourceKey::Memory(data.digest()),
        }
    }

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

/// Frozen PCM represented by a canonical F32 WAV and its SHA-256.
/// Surround sources retain their explicit, validated speaker layout.
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
    channel_mask: Option<u32>,
    sample_rate: u32,
    sample_frames: usize,
}

impl fmt::Debug for SoundData {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SoundData")
            .field("digest", &self.digest)
            .field("bytes", &self.bytes.len())
            .field("channels", &self.channels)
            .field("channel_mask", &self.channel_mask)
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
        audio.channel_layout().map_err(SoundDataError::Decode)?;
        // Explicit mono/stereo layouts have the same canonical identity as
        // their historical implicit forms. Surround masks remain part of the
        // source: identical samples in different speaker positions are distinct.
        let channel_mask = if audio.channels > 2 {
            audio.channel_mask
        } else {
            None
        };
        let header_bytes = if channel_mask.is_some() { 68 } else { 44 };
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
            .and_then(|n| n.checked_add(header_bytes));
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
        let canonical = WavAudio {
            channels: audio.channels,
            channel_mask,
            sample_rate: audio.sample_rate,
            format: SampleFormat::F32,
            samples,
        };
        let bytes: Arc<[u8]> = encode_wav_with_layout(&canonical, SampleFormat::F32)
            .map_err(SoundDataError::Decode)?
            .into();
        Ok(Self {
            digest: sha256(&bytes),
            bytes,
            channels: audio.channels,
            channel_mask,
            sample_rate: audio.sample_rate,
            sample_frames: audio.samples.len() / usize::from(audio.channels),
        })
    }

    /// Decode native PCM WAV and freeze its normalized samples canonically.
    /// No subprocess or filesystem capability is used. Source bytes and sample
    /// allocation are bounded before decoding; the exact canonical byte budget
    /// includes the speaker-layout header and is checked before encoding.
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

    /// Number of interleaved channels in the validated speaker layout.
    #[must_use]
    pub const fn channels(&self) -> u16 {
        self.channels
    }

    /// Explicit surround speaker mask; mono/stereo use their canonical implicit layout.
    #[must_use]
    pub const fn channel_mask(&self) -> Option<u32> {
        self.channel_mask
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
    use fmn_codec::encode_wav;

    fn pcm() -> WavAudio {
        WavAudio {
            channels: 2,
            channel_mask: None,
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
    fn explicit_mono_and_stereo_keep_the_existing_canonical_identity() {
        for (channels, mask) in [(1, 0x4), (2, 0x3)] {
            let implicit = WavAudio { channels, ..pcm() };
            let explicit = WavAudio {
                channel_mask: Some(mask),
                ..implicit.clone()
            };
            let frozen = SoundData::from_audio(&implicit, SoundDataLimits::default()).unwrap();
            assert_eq!(
                SoundData::from_audio(&explicit, SoundDataLimits::default()).unwrap(),
                frozen
            );
            let extensible = encode_wav_with_layout(&explicit, SampleFormat::F32).unwrap();
            assert_eq!(
                SoundData::from_wav_bytes(&extensible, SoundDataLimits::default()).unwrap(),
                frozen
            );
            assert_eq!(frozen.channel_mask(), None);
            assert_eq!(frozen.bytes().len(), 44 + implicit.samples.len() * 4);
        }
    }

    #[test]
    fn surround_canonicalization_preserves_layout_and_bounds_its_header() {
        let audio = WavAudio {
            channels: 6,
            channel_mask: Some(0x3f),
            sample_rate: 48_000,
            format: SampleFormat::F32,
            samples: vec![0.0, -0.0, 0.5, -0.25, 0.125, -0.125],
        };
        let frozen = SoundData::from_audio(&audio, SoundDataLimits::default()).unwrap();
        assert_eq!(frozen.channel_mask(), Some(0x3f));
        assert_eq!((frozen.channels(), frozen.sample_frames()), (6, 1));
        assert_eq!(frozen.bytes().len(), 68 + 6 * 4);
        let decoded = decode_wav(frozen.bytes(), &WavLimits::default()).unwrap();
        assert_eq!(decoded.channel_mask, Some(0x3f));
        assert_eq!(decoded.samples, audio.samples);
        assert_eq!(decoded.samples[1].to_bits(), 0);
        assert!(Arc::ptr_eq(frozen.bytes(), frozen.clone().bytes()));

        let compact = encode_wav_with_layout(&audio, SampleFormat::S16).unwrap();
        assert_eq!(
            SoundData::from_wav_bytes(&compact, SoundDataLimits::default()).unwrap(),
            frozen
        );
        let side = WavAudio {
            channel_mask: Some(0x60f),
            ..audio.clone()
        };
        assert_ne!(
            SoundData::from_audio(&side, SoundDataLimits::default())
                .unwrap()
                .digest(),
            frozen.digest(),
            "side and back speakers must not share a source identity"
        );
        for channel_mask in [None, Some(0x18f), Some(0x7)] {
            assert!(matches!(
                SoundData::from_audio(
                    &WavAudio {
                        channel_mask,
                        ..audio.clone()
                    },
                    SoundDataLimits::default()
                ),
                Err(SoundDataError::Decode(_))
            ));
        }
        for max_bytes in [compact.len(), frozen.bytes().len() - 1] {
            assert!(matches!(
                SoundData::from_wav_bytes(
                    &compact,
                    SoundDataLimits {
                        max_bytes,
                        max_samples: 6
                    }
                ),
                Err(SoundDataError::LimitExceeded {
                    resource: "canonical WAV bytes",
                    ..
                })
            ));
        }
        assert_eq!(
            SoundData::from_wav_bytes(
                &compact,
                SoundDataLimits {
                    max_bytes: 92,
                    max_samples: 6
                }
            )
            .unwrap(),
            frozen
        );
    }

    #[test]
    fn malformed_pcm_and_expansion_budgets_are_refused_before_freezing() {
        for channels in [0, 3, u16::MAX] {
            assert!(matches!(
                SoundData::from_audio(&WavAudio { channels, ..pcm() }, SoundDataLimits::default()),
                Err(SoundDataError::Decode(_))
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
