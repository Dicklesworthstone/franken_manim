//! FMTL/1 minor 2: immutable pictures plus an already mixed native PCM track.
//!
//! The nested picture document is verbatim minor 0 or 1, not another audio
//! envelope. Its existing clock, camera codec and reconstruction law stay intact.
use fmn_hash::serial::{Limits, Reader, Schema, UnknownPolicy, Writer};

use super::{
    BundleReadError, CAMERA_TIMELINE_BUNDLE_SCHEMA, DEFAULT_MAX_BUNDLE_BYTES, TimelineBundle,
    bundle_engine_version,
};

/// One sound-bearing FMTL document, containing one ordinary visual document.
pub const SOUNDTRACK_TIMELINE_BUNDLE_SCHEMA: Schema = Schema::new(*b"FMTL", 1, 0, 2);

/// A final, normalized mono/stereo Float32 soundtrack beginning at movie time zero.
///
/// No paths, codecs, source callbacks, gain commands or resampling instructions
/// survive export. The sample rate is independent of the picture FPS. Samples
/// are interleaved; a short track ends in silence and a long track retains its
/// tail. Consumers decide presentation duration explicitly, never by resampling
/// picture frames. All floats are finite in [-1, 1], and zero is canonical.
#[derive(Debug, Clone, PartialEq)]
pub struct BundleSoundtrack {
    sample_rate: u32,
    channels: u16,
    samples: Vec<f32>,
}

impl BundleSoundtrack {
    /// Adopt normalized PCM without copying it. No external asset is accessed.
    ///
    /// # Errors
    /// Refuses invalid channel/rate/packing, non-finite or unclamped samples,
    /// and sample storage above the production bundle byte ceiling.
    pub fn new(
        sample_rate: u32,
        channels: u16,
        mut samples: Vec<f32>,
    ) -> Result<Self, BundleReadError> {
        validate_layout(sample_rate, channels)?;
        if !samples.len().is_multiple_of(usize::from(channels)) {
            return Err(invalid("interleaved PCM must contain whole channel frames"));
        }
        if samples.len() > DEFAULT_MAX_BUNDLE_BYTES / 4 {
            return Err(invalid("PCM exceeds the bundle sample budget"));
        }
        for sample in &mut samples {
            if !sample.is_finite() || !(-1.0..=1.0).contains(sample) {
                return Err(invalid(
                    "PCM samples must be finite and normalized to [-1, 1]",
                ));
            }
            if *sample == 0.0 {
                *sample = 0.0;
            }
        }
        Ok(Self {
            sample_rate,
            channels,
            samples,
        })
    }

    /// Sample frames per second; independent of the picture clock.
    #[must_use]
    pub const fn sample_rate(&self) -> u32 {
        self.sample_rate
    }
    /// One for mono or two for stereo, in interleaved left/right order.
    #[must_use]
    pub const fn channels(&self) -> u16 {
        self.channels
    }
    /// Count of complete sample frames, not interleaved channel samples.
    #[must_use]
    pub fn sample_frames(&self) -> u64 {
        (self.samples.len() / usize::from(self.channels)) as u64
    }
    /// The immutable canonical PCM, suitable for native output or host playback.
    #[must_use]
    pub fn samples(&self) -> &[f32] {
        &self.samples
    }
    /// Transfer PCM ownership to an audio-aware consumer without a second copy.
    #[must_use]
    pub fn into_samples(self) -> Vec<f32> {
        self.samples
    }
}

fn invalid(reason: &'static str) -> BundleReadError {
    BundleReadError::Soundtrack(reason)
}

fn validate_layout(sample_rate: u32, channels: u16) -> Result<(), BundleReadError> {
    if !matches!(channels, 1 | 2) {
        return Err(invalid("only mono and stereo PCM layouts are supported"));
    }
    if sample_rate == 0 || sample_rate.checked_mul(u32::from(channels) * 4).is_none() {
        return Err(invalid(
            "sample rate must be nonzero and fit a native Float32 PCM byte rate",
        ));
    }
    Ok(())
}

fn envelope_limits(max_bytes: usize) -> Limits {
    Limits {
        max_total: max_bytes.min(DEFAULT_MAX_BUNDLE_BYTES),
        max_field: DEFAULT_MAX_BUNDLE_BYTES,
    }
}

/// Package an existing visual bundle and final PCM into one content-bound artifact.
///
/// The original visual bytes are unchanged. The only enlarged field limit is
/// the nested *document*: its own reader still enforces the original 64 MiB
/// snapshot-field bound. The complete envelope keeps the 256 MiB ceiling.
/// Export performs no scene work, filesystem access, decoding of media, or mix.
///
/// # Errors
/// Refuses invalid picture input, nesting, allocation or the explicit output cap.
/// An old strict reader rejects minor 2 rather than silently omitting sound.
pub fn attach_bundle_soundtrack(
    pictures: &[u8],
    soundtrack: &BundleSoundtrack,
    max_bytes: usize,
) -> Result<Vec<u8>, BundleReadError> {
    // Refuse recursive audio envelopes before entering the shared visual reader.
    Reader::open(
        pictures,
        CAMERA_TIMELINE_BUNDLE_SCHEMA,
        Limits::DEFAULT,
        UnknownPolicy::Strict,
    )
    .map_err(BundleReadError::Malformed)?;
    let identity = bundle_engine_version();
    let mut header = Writer::new(SOUNDTRACK_TIMELINE_BUNDLE_SCHEMA);
    header
        .put_str(&identity)
        .put_bytes(&[])
        .put_u32(soundtrack.sample_rate)
        .put_u16(soundtrack.channels)
        .put_u64(soundtrack.sample_frames());
    let overhead = header.finish().map_err(BundleReadError::Malformed)?.len();
    let needed = soundtrack
        .samples
        .len()
        .checked_mul(4)
        .and_then(|n| n.checked_add(pictures.len()))
        .and_then(|n| n.checked_add(overhead))
        .unwrap_or(usize::MAX);
    let limit = max_bytes.min(DEFAULT_MAX_BUNDLE_BYTES);
    if needed > limit {
        return Err(BundleReadError::Malformed(
            fmn_hash::SerialError::SizeLimit { needed, limit },
        ));
    }
    TimelineBundle::from_bytes(pictures)?;
    let mut writer = Writer::with_limits(
        SOUNDTRACK_TIMELINE_BUNDLE_SCHEMA,
        envelope_limits(max_bytes),
    );
    writer.put_str(&bundle_engine_version());
    writer.put_bytes(pictures);
    writer.put_u32(soundtrack.sample_rate);
    writer.put_u16(soundtrack.channels);
    writer.put_u64(soundtrack.sample_frames());
    for &sample in &soundtrack.samples {
        writer.put_finite_f32(sample);
    }
    writer.finish().map_err(BundleReadError::Malformed)
}

pub(super) fn read_envelope(bytes: &[u8]) -> Result<TimelineBundle, BundleReadError> {
    // The caller already identified minor 2 with the ordinary strict reader.
    // Reopen with the document-sized field allowance only for this envelope.
    let mut reader = Reader::open(
        bytes,
        SOUNDTRACK_TIMELINE_BUNDLE_SCHEMA,
        envelope_limits(DEFAULT_MAX_BUNDLE_BYTES),
        UnknownPolicy::Strict,
    )
    .map_err(BundleReadError::Malformed)?;
    let wanted = reader.get_str().map_err(BundleReadError::Malformed)?;
    let found = bundle_engine_version();
    if wanted != found {
        return Err(BundleReadError::EngineMismatch {
            wanted: wanted.to_owned(),
            found,
        });
    }
    let pictures = reader.get_bytes().map_err(BundleReadError::Malformed)?;
    Reader::open(
        pictures,
        CAMERA_TIMELINE_BUNDLE_SCHEMA,
        Limits::DEFAULT,
        UnknownPolicy::Strict,
    )
    .map_err(BundleReadError::Malformed)?;
    let sample_rate = reader.get_u32().map_err(BundleReadError::Malformed)?;
    let channels = reader.get_u16().map_err(BundleReadError::Malformed)?;
    validate_layout(sample_rate, channels)?;
    let frames = reader.get_u64().map_err(BundleReadError::Malformed)?;
    let samples = frames
        .checked_mul(u64::from(channels))
        .and_then(|n| usize::try_from(n).ok())
        .ok_or_else(|| invalid("PCM sample count exceeds the host width"))?;
    if samples.checked_mul(4) != Some(reader.remaining()) {
        return Err(invalid("PCM sample count does not match the exact payload"));
    }
    // Count and complete payload are verified before any input-sized allocation.
    let mut pcm = Vec::new();
    pcm.try_reserve_exact(samples)
        .map_err(|_| BundleReadError::AllocationFailed {
            context: "bundle soundtrack",
            requested: samples,
        })?;
    for _ in 0..samples {
        let sample = reader
            .get_finite_f32()
            .map_err(BundleReadError::Malformed)?;
        if !(-1.0..=1.0).contains(&sample) {
            return Err(invalid("PCM sample lies outside [-1, 1]"));
        }
        pcm.push(sample);
    }
    reader.finish().map_err(BundleReadError::Malformed)?;
    let mut bundle = TimelineBundle::from_bytes(pictures)?;
    bundle.soundtrack = Some(BundleSoundtrack {
        sample_rate,
        channels,
        samples: pcm,
    });
    Ok(bundle)
}

impl TimelineBundle {
    /// Whether this artifact requires audio-aware playback.
    #[must_use]
    pub const fn has_soundtrack(&self) -> bool {
        self.soundtrack.is_some()
    }
    /// Read the soundtrack without weakening picture-only playback admission.
    #[must_use]
    pub fn soundtrack(&self) -> Option<&BundleSoundtrack> {
        self.soundtrack.as_ref()
    }
    /// Explicitly assume responsibility for sound, leaving the visual document.
    ///
    /// Only after this transfer can ordinary picture/worker APIs be used. An
    /// audio-aware host schedules the returned PCM from movie time zero; a user
    /// intentionally exporting stills may explicitly discard it. Merely loading
    /// a sound-bearing artifact never authorizes silent playback.
    #[must_use]
    pub fn take_soundtrack(&mut self) -> Option<BundleSoundtrack> {
        self.soundtrack.take()
    }

    pub(super) fn require_picture_only(&self) -> Result<(), BundleReadError> {
        if self.soundtrack.is_some() {
            Err(BundleReadError::SoundtrackRequired)
        } else {
            Ok(())
        }
    }
}
