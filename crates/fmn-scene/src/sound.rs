//! Immutable, content-addressed sources for scene sound cues (§14.5).
//!
//! File paths stay unresolved until the composition root supplies filesystem
//! capabilities. Caller-owned PCM is admitted here and frozen as canonical
//! native WAV, so all front doors use Reel's existing decoder and mixer.

use std::borrow::Cow;
use std::collections::{BTreeMap, BTreeSet};
use std::ffi::{OsStr, OsString};
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use fmn_codec::{SampleFormat, WavAudio, WavError, WavLimits, decode_wav, encode_wav_with_layout};
use fmn_hash::{Digest, Limits, Reader, Schema, SerialError, UnknownPolicy, Writer, sha256};

use crate::{SoundRequest, SoundTime};
use fmn_anim::RationalTime;

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

/// Canonical companion to SceneState for insertion-ordered sound history.
pub const SOUND_STATE_SCHEMA: Schema = Schema::new(*b"FMSS", 1, 1, 0);

/// Compact audio identity for verified replay; it deliberately cannot restore PCM.
pub const SOUND_STATE_IDENTITY_SCHEMA: Schema = Schema::new(*b"FMSI", 1, 1, 0);

/// Independent count ceiling for sound checkpoints, including zero-length cues.
pub const MAX_SOUND_STATE_REQUESTS: usize = 65_536;

/// A refused sound checkpoint never changes the installed scene queue.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SoundStateError {
    /// Framing, checksum, finite-number, or byte-budget failure.
    Serialization(SerialError),
    /// Invalid or over-budget embedded PCM.
    Source(SoundDataError),
    /// Invalid cue metadata or a noncanonical source table.
    Invalid(&'static str),
    /// A count or decoded-allocation budget was exceeded.
    LimitExceeded {
        /// The resource whose admission failed.
        resource: &'static str,
        /// The receiving limit.
        limit: usize,
    },
    /// A bounded allocation could not be reserved.
    Allocation,
}

impl fmt::Display for SoundStateError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Serialization(error) => write!(f, "sound checkpoint: {error}"),
            Self::Source(error) => write!(f, "sound checkpoint source: {error}"),
            Self::Invalid(message) => write!(f, "invalid sound checkpoint: {message}"),
            Self::LimitExceeded { resource, limit } => {
                write!(f, "sound checkpoint exceeds {resource} budget of {limit}")
            }
            Self::Allocation => f.write_str("sound checkpoint allocation failed"),
        }
    }
}

impl std::error::Error for SoundStateError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Serialization(error) => Some(error),
            Self::Source(error) => Some(error),
            _ => None,
        }
    }
}

impl From<SerialError> for SoundStateError {
    fn from(error: SerialError) -> Self {
        Self::Serialization(error)
    }
}

impl From<SoundDataError> for SoundStateError {
    fn from(error: SoundDataError) -> Self {
        Self::Source(error)
    }
}

/// Validated audio history, separate from Marionette's visual SceneState.
///
/// Sources are stored once in first-use order. Memory sources share immutable
/// canonical WAV bytes after restore; file paths remain lazy authoring data.
/// Capture and restore perform no filesystem access or media decoding through
/// an external tool. File contents remain part of the host's input closure.
/// UTF-8 paths are portable; non-UTF-8 Unix and Windows paths use explicit
/// platform tags and are refused on an incompatible receiving platform.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SoundState {
    requests: Vec<SoundRequest>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum StateSourceKey<'a> {
    // OsStr preserves the authored spelling. Path equality would collapse
    // components such as `a/./b`, which are not identical authoring records.
    File(&'a OsStr),
    Memory(Digest),
}

fn state_source_key(source: &SoundSource) -> StateSourceKey<'_> {
    match source {
        SoundSource::File(path) => StateSourceKey::File(path.as_os_str()),
        SoundSource::Memory(data) => StateSourceKey::Memory(data.digest()),
    }
}

fn charge_sound_paths(
    source: &SoundSource,
    total: &mut usize,
    max_bytes: usize,
) -> Result<(), SoundStateError> {
    if let SoundSource::File(path) = source {
        *total =
            total
                .checked_add(path.as_os_str().len())
                .ok_or(SoundStateError::LimitExceeded {
                    resource: "decoded file path bytes",
                    limit: max_bytes,
                })?;
        if *total > max_bytes {
            return Err(SoundStateError::LimitExceeded {
                resource: "decoded file path bytes",
                limit: max_bytes,
            });
        }
    }
    Ok(())
}

fn put_sound_path(
    writer: &mut Writer,
    path: &Path,
    max_bytes: usize,
) -> Result<(), SoundStateError> {
    if let Some(text) = path.to_str() {
        writer.put_u8(0).put_str(text);
        return Ok(());
    }
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        let _ = max_bytes;
        writer.put_u8(1).put_bytes(path.as_os_str().as_bytes());
        Ok(())
    }
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        let units = path.as_os_str().encode_wide().count();
        let count = units.checked_mul(2).filter(|&n| n <= max_bytes).ok_or(
            SoundStateError::LimitExceeded {
                resource: "path bytes",
                limit: max_bytes,
            },
        )?;
        let mut bytes = Vec::new();
        bytes
            .try_reserve_exact(count)
            .map_err(|_| SoundStateError::Allocation)?;
        for unit in path.as_os_str().encode_wide() {
            bytes.extend_from_slice(&unit.to_le_bytes());
        }
        writer.put_u8(2).put_bytes(&bytes);
        Ok(())
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = (writer, max_bytes);
        Err(SoundStateError::Invalid("unsupported native path encoding"))
    }
}

fn get_sound_path(reader: &mut Reader<'_>) -> Result<PathBuf, SoundStateError> {
    let tag = reader.get_u8()?;
    let bytes = reader.get_bytes()?;
    if tag == 0 {
        let text = std::str::from_utf8(bytes)
            .map_err(|_| SoundStateError::Invalid("file path is not UTF-8"))?;
        let mut path = OsString::new();
        path.try_reserve_exact(text.len())
            .map_err(|_| SoundStateError::Allocation)?;
        path.push(text);
        return Ok(PathBuf::from(path));
    }
    #[cfg(unix)]
    if tag == 1 {
        use std::os::unix::ffi::OsStringExt;
        if std::str::from_utf8(bytes).is_ok() {
            return Err(SoundStateError::Invalid("noncanonical Unix path tag"));
        }
        let mut owned = Vec::new();
        owned
            .try_reserve_exact(bytes.len())
            .map_err(|_| SoundStateError::Allocation)?;
        owned.extend_from_slice(bytes);
        return Ok(PathBuf::from(OsString::from_vec(owned)));
    }
    #[cfg(windows)]
    if tag == 2 {
        use std::os::windows::ffi::OsStringExt;
        if !bytes.len().is_multiple_of(2) {
            return Err(SoundStateError::Invalid(
                "incomplete Windows path code unit",
            ));
        }
        let mut units = Vec::new();
        units
            .try_reserve_exact(bytes.len() / 2)
            .map_err(|_| SoundStateError::Allocation)?;
        units.extend(
            bytes
                .chunks_exact(2)
                .map(|pair| u16::from_le_bytes([pair[0], pair[1]])),
        );
        let path = PathBuf::from(OsString::from_wide(&units));
        if path.to_str().is_some() {
            return Err(SoundStateError::Invalid("noncanonical Windows path tag"));
        }
        return Ok(path);
    }
    Err(SoundStateError::Invalid("unsupported native path encoding"))
}

fn copy_sound_source(source: &SoundSource) -> Result<SoundSource, SoundStateError> {
    Ok(match source {
        SoundSource::Memory(data) => SoundSource::Memory(data.clone()),
        SoundSource::File(path) => {
            let mut owned = OsString::new();
            owned
                .try_reserve_exact(path.as_os_str().len())
                .map_err(|_| SoundStateError::Allocation)?;
            owned.push(path.as_os_str());
            SoundSource::File(PathBuf::from(owned))
        }
    })
}

fn get_sound_time(reader: &mut Reader<'_>) -> Result<RationalTime, SoundStateError> {
    let frames = reader.get_i64()?;
    let fps = reader.get_u32()?;
    if fps == 0 {
        return Err(SoundStateError::Invalid(
            "rational time rate must be positive",
        ));
    }
    Ok(RationalTime::zero(fps) + frames)
}

fn put_sound_gain(writer: &mut Writer, gain: Option<f64>) {
    writer.put_bool(gain.is_some());
    if let Some(gain) = gain {
        writer.put_finite_f64(gain);
    }
}

fn get_sound_gain(reader: &mut Reader<'_>) -> Result<Option<f64>, SoundStateError> {
    Ok(if reader.get_bool()? {
        Some(reader.get_finite_f64()?)
    } else {
        None
    })
}

impl SoundState {
    /// The original cue order and exact frame/sample grids.
    #[must_use]
    pub fn requests(&self) -> &[SoundRequest] {
        &self.requests
    }

    /// Whether the saved scene had no authored cues.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.requests.is_empty()
    }

    /// Encode this validated queue within the receiving document budget.
    /// Finite floating values use the ordinary canonical signed-zero policy.
    ///
    /// # Errors
    /// A byte/count/allocation budget failure.
    pub fn to_bytes(&self, max_bytes: usize) -> Result<Vec<u8>, SoundStateError> {
        Self::encode_requests(&self.requests, max_bytes)
    }

    /// Encode the exact authored audio identity without embedding PCM again.
    ///
    /// Memory sources contribute their canonical content digest and metadata;
    /// file paths, cue order, timing grids, and gains remain exact. This is a
    /// comparison document for verified replay, not a restorable SoundState.
    /// Its size depends on source/cue metadata rather than soundtrack duration.
    ///
    /// # Errors
    /// A metadata byte/count/allocation budget failure.
    pub fn identity_bytes(&self, max_bytes: usize) -> Result<Vec<u8>, SoundStateError> {
        Self::encode_identity_requests(&self.requests, max_bytes)
    }

    pub(crate) fn into_requests(self) -> Vec<SoundRequest> {
        self.requests
    }

    pub(crate) fn encode_requests(
        requests: &[SoundRequest],
        max_bytes: usize,
    ) -> Result<Vec<u8>, SoundStateError> {
        Self::encode(requests, max_bytes, true)
    }

    pub(crate) fn encode_identity_requests(
        requests: &[SoundRequest],
        max_bytes: usize,
    ) -> Result<Vec<u8>, SoundStateError> {
        Self::encode(requests, max_bytes, false)
    }

    fn encode(
        requests: &[SoundRequest],
        max_bytes: usize,
        include_pcm: bool,
    ) -> Result<Vec<u8>, SoundStateError> {
        // 56 framing bytes, two u32 counts, and at least 27 bytes per cue.
        // Refuse an impossible request count before reserving any table.
        if requests.len() > MAX_SOUND_STATE_REQUESTS
            || requests.len() > max_bytes.saturating_sub(64) / 27
        {
            return Err(SoundStateError::LimitExceeded {
                resource: "cue count",
                limit: MAX_SOUND_STATE_REQUESTS.min(max_bytes.saturating_sub(64) / 27),
            });
        }
        let mut sources = Vec::new();
        let mut indices = Vec::new();
        sources
            .try_reserve_exact(requests.len())
            .map_err(|_| SoundStateError::Allocation)?;
        indices
            .try_reserve_exact(requests.len())
            .map_err(|_| SoundStateError::Allocation)?;
        let mut keys = BTreeMap::new();
        let mut path_bytes = 0;
        for request in requests {
            if request.time.fps() == 0
                || matches!(request.absolute_time, Some(SoundTime::Rational(time)) if time.fps() == 0)
            {
                return Err(SoundStateError::Invalid(
                    "rational time rate must be positive",
                ));
            }
            charge_sound_paths(&request.source, &mut path_bytes, max_bytes)?;
            let next = sources.len() as u32;
            let index = *keys
                .entry(state_source_key(&request.source))
                .or_insert_with(|| {
                    sources.push(&request.source);
                    next
                });
            indices.push(index);
        }
        let mut writer = Writer::with_limits(
            if include_pcm {
                SOUND_STATE_SCHEMA
            } else {
                SOUND_STATE_IDENTITY_SCHEMA
            },
            Limits {
                max_total: max_bytes,
                max_field: max_bytes,
            },
        );
        writer.put_u32(sources.len() as u32);
        for source in sources {
            match source {
                SoundSource::File(path) => {
                    writer.put_u8(0);
                    put_sound_path(&mut writer, path, max_bytes)?;
                }
                SoundSource::Memory(data) => {
                    writer.put_u8(1).put_digest(&data.digest());
                    if include_pcm {
                        writer.put_bytes(data.bytes());
                    } else {
                        writer
                            .put_u16(data.channels())
                            .put_u32(data.sample_rate())
                            .put_u64(data.sample_frames() as u64)
                            .put_bool(data.channel_mask().is_some());
                        if let Some(mask) = data.channel_mask() {
                            writer.put_u32(mask);
                        }
                    }
                }
            }
        }
        writer.put_u32(requests.len() as u32);
        for (request, index) in requests.iter().zip(indices) {
            writer
                .put_u32(index)
                .put_i64(request.time.frames())
                .put_u32(request.time.fps())
                .put_finite_f64(request.time_offset);
            match request.absolute_time {
                None => {
                    writer.put_u8(0);
                }
                Some(SoundTime::Seconds(time)) => {
                    writer.put_u8(1).put_finite_f64(time);
                }
                Some(SoundTime::Rational(time)) => {
                    writer.put_u8(2).put_i64(time.frames()).put_u32(time.fps());
                }
            }
            put_sound_gain(&mut writer, request.gain);
            put_sound_gain(&mut writer, request.gain_to_background);
        }
        Ok(writer.finish()?)
    }

    /// Decode a complete sound checkpoint before any Scene mutation.
    ///
    /// `max_bytes` bounds the document, embedded WAV expansion, and aggregate
    /// copied file-path bytes. Cue/source counts have an independent ceiling;
    /// duplicate, unused and non-first-use source rows are noncanonical.
    ///
    /// # Errors
    /// Bad framing/checksum, invalid metadata or source indices, incompatible
    /// native path encoding, noncanonical/invalid PCM, or a resource refusal.
    pub fn from_bytes(bytes: &[u8], max_bytes: usize) -> Result<Self, SoundStateError> {
        let mut reader = Reader::open(
            bytes,
            SOUND_STATE_SCHEMA,
            Limits {
                max_total: max_bytes,
                max_field: max_bytes,
            },
            UnknownPolicy::Strict,
        )?;
        let count = reader.get_u32()? as usize;
        if count > MAX_SOUND_STATE_REQUESTS || count > reader.remaining().saturating_sub(4) / 10 {
            return Err(SoundStateError::Invalid(
                "source count exceeds bounded payload",
            ));
        }
        let mut sources = Vec::new();
        sources
            .try_reserve_exact(count)
            .map_err(|_| SoundStateError::Allocation)?;
        for _ in 0..count {
            let source = match reader.get_u8()? {
                0 => SoundSource::File(get_sound_path(&mut reader)?),
                1 => {
                    let digest = reader.get_digest()?;
                    let wav = reader.get_bytes()?;
                    if sha256(wav) != digest {
                        return Err(SoundStateError::Invalid("memory source digest mismatch"));
                    }
                    let data = SoundData::from_wav_bytes(
                        wav,
                        SoundDataLimits {
                            max_bytes,
                            max_samples: max_bytes.saturating_sub(44) / 4,
                        },
                    )?;
                    if data.bytes().as_ref() != wav {
                        return Err(SoundStateError::Invalid(
                            "memory source is not canonical PCM",
                        ));
                    }
                    SoundSource::Memory(data)
                }
                _ => return Err(SoundStateError::Invalid("sound source tag")),
            };
            sources.push(source);
        }
        let mut keys = BTreeSet::new();
        for source in &sources {
            if !keys.insert(state_source_key(source)) {
                return Err(SoundStateError::Invalid("duplicate sound source"));
            }
        }
        let count = reader.get_u32()? as usize;
        if count > MAX_SOUND_STATE_REQUESTS || count > reader.remaining() / 27 {
            return Err(SoundStateError::Invalid(
                "cue count exceeds bounded payload",
            ));
        }
        let mut requests = Vec::new();
        requests
            .try_reserve_exact(count)
            .map_err(|_| SoundStateError::Allocation)?;
        let mut used_sources = 0;
        let mut path_bytes = 0;
        for _ in 0..count {
            let index = reader.get_u32()? as usize;
            if index > used_sources || index >= sources.len() {
                return Err(SoundStateError::Invalid("source index or first-use order"));
            }
            if index == used_sources {
                used_sources += 1;
            }
            let time = get_sound_time(&mut reader)?;
            let time_offset = reader.get_finite_f64()?;
            let absolute_time = match reader.get_u8()? {
                0 => None,
                1 => Some(SoundTime::Seconds(reader.get_finite_f64()?)),
                2 => Some(SoundTime::Rational(get_sound_time(&mut reader)?)),
                _ => return Err(SoundStateError::Invalid("sound placement tag")),
            };
            let gain = get_sound_gain(&mut reader)?;
            let gain_to_background = get_sound_gain(&mut reader)?;
            charge_sound_paths(&sources[index], &mut path_bytes, max_bytes)?;
            requests.push(SoundRequest {
                source: copy_sound_source(&sources[index])?,
                time,
                time_offset,
                absolute_time,
                gain,
                gain_to_background,
            });
        }
        if used_sources != sources.len() {
            return Err(SoundStateError::Invalid("unused sound source"));
        }
        reader.finish()?;
        Ok(Self { requests })
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

    fn checkpoint_cue(source: SoundSource, frame: i64) -> SoundRequest {
        SoundRequest {
            source,
            time: RationalTime::zero(60) + frame,
            time_offset: -0.125,
            absolute_time: None,
            gain: Some(-3.0),
            gain_to_background: None,
        }
    }

    fn sound_writer() -> Writer {
        Writer::with_limits(
            SOUND_STATE_SCHEMA,
            Limits {
                max_total: 8192,
                max_field: 8192,
            },
        )
    }

    fn put_minimal_cue(writer: &mut Writer, index: u32) {
        writer
            .put_u32(index)
            .put_i64(3)
            .put_u32(60)
            .put_finite_f64(0.0)
            .put_u8(0)
            .put_bool(false)
            .put_bool(false);
    }

    #[test]
    fn sound_identity_is_compact_but_still_binds_pcm_metadata_and_exact_cue_grids() {
        let mut audio = WavAudio {
            channels: 1,
            channel_mask: None,
            sample_rate: 48_000,
            format: SampleFormat::F32,
            samples: vec![0.125; 4096],
        };
        let data = SoundData::from_audio(&audio, SoundDataLimits::default()).unwrap();
        let cue = checkpoint_cue(SoundSource::Memory(data), 1);
        let identity =
            SoundState::encode_identity_requests(std::slice::from_ref(&cue), 512).unwrap();
        assert!(identity.len() < 512);
        assert!(identity.starts_with(b"FMSI"));
        assert!(SoundState::encode_requests(std::slice::from_ref(&cue), 512).is_err());
        assert!(SoundState::from_bytes(&identity, 512).is_err());
        let full = SoundState::encode_requests(std::slice::from_ref(&cue), 65_536).unwrap();
        assert_eq!(
            SoundState::from_bytes(&full, 65_536)
                .unwrap()
                .identity_bytes(512)
                .unwrap(),
            identity,
        );

        let mut changed = cue.clone();
        changed.time = RationalTime::zero(120) + 2;
        assert_eq!(changed.time, cue.time);
        assert_ne!(
            SoundState::encode_identity_requests(&[changed], 512).unwrap(),
            identity,
            "equal rational values must retain their original authoring grids",
        );
        let mut changed = cue.clone();
        changed.gain_to_background = Some(-6.0);
        assert_ne!(
            SoundState::encode_identity_requests(&[changed], 512).unwrap(),
            identity,
        );
        audio.samples[4095] = -0.25;
        let changed = checkpoint_cue(
            SoundSource::Memory(SoundData::from_audio(&audio, SoundDataLimits::default()).unwrap()),
            1,
        );
        assert_ne!(
            SoundState::encode_identity_requests(&[changed], 512).unwrap(),
            identity,
            "compact replay identity must authenticate the actual PCM",
        );
        audio.samples[4095] = 0.125;
        audio.sample_rate = 44_100;
        let changed = checkpoint_cue(
            SoundSource::Memory(SoundData::from_audio(&audio, SoundDataLimits::default()).unwrap()),
            1,
        );
        assert_ne!(
            SoundState::encode_identity_requests(&[changed], 512).unwrap(),
            identity,
        );
    }

    #[test]
    fn sound_state_preserves_source_spelling_exact_grids_and_shared_pcm() {
        let data = SoundData::from_audio(&pcm(), SoundDataLimits::default()).unwrap();
        let mut requests = vec![
            checkpoint_cue(SoundSource::File("missing/narration.wav".into()), 3),
            checkpoint_cue(SoundSource::Memory(data.clone()), 4),
            checkpoint_cue(SoundSource::Memory(data), 7),
            checkpoint_cue(SoundSource::File("missing/./narration.wav".into()), 9),
        ];
        requests[1].absolute_time = Some(SoundTime::Rational(RationalTime::zero(48_000) + 1729));
        requests[1].time_offset = -0.0;
        requests[1].gain_to_background = Some(-6.0);
        requests[2].absolute_time = Some(SoundTime::Seconds(2.125));
        requests[2].gain = None;
        let bytes = SoundState::encode_requests(&requests, 8192).unwrap();
        let decoded = SoundState::from_bytes(&bytes, 8192).unwrap();
        assert_eq!(decoded.requests(), requests);
        for (actual, expected) in decoded.requests().iter().zip(&requests) {
            assert_eq!(
                (actual.time.frames(), actual.time.fps()),
                (expected.time.frames(), expected.time.fps()),
            );
            if let (SoundSource::File(actual), SoundSource::File(expected)) =
                (&actual.source, &expected.source)
            {
                assert_eq!(actual.as_os_str(), expected.as_os_str());
            }
        }
        let Some(SoundTime::Rational(time)) = decoded.requests()[1].absolute_time else {
            panic!("absolute placement lost its rational sample grid");
        };
        assert_eq!((time.frames(), time.fps()), (1729, 48_000));
        assert_eq!(decoded.requests()[1].time_offset.to_bits(), 0);
        let (SoundSource::Memory(first), SoundSource::Memory(second)) =
            (&decoded.requests()[1].source, &decoded.requests()[2].source)
        else {
            panic!("memory cues became file paths");
        };
        assert!(Arc::ptr_eq(first.bytes(), second.bytes()));
        let mut reader = Reader::open(
            &bytes,
            SOUND_STATE_SCHEMA,
            Limits::default(),
            UnknownPolicy::Strict,
        )
        .unwrap();
        assert_eq!(reader.get_u32().unwrap(), 3);
        assert_eq!(decoded.to_bytes(8192).unwrap(), bytes);
    }

    #[test]
    fn sound_restore_is_atomic_preserves_the_scene_clock_and_can_clear_history() {
        let mut scene = crate::Scene::new(
            crate::RuntimeConfig {
                fps: 60,
                ..crate::RuntimeConfig::default()
            },
            7,
        )
        .unwrap();
        scene.wait_frames(16, &mut crate::NullSceneSink).unwrap();
        scene.add_sound("old.wav", 0.0, None, None).unwrap();
        let before = scene.sound_state_bytes(8192).unwrap();
        let clock = (scene.time().frames(), scene.time().fps());
        let cue = checkpoint_cue(SoundSource::File("missing/new.wav".into()), 4);
        let bytes = SoundState::encode_requests(std::slice::from_ref(&cue), 8192).unwrap();
        for length in 0..bytes.len() {
            assert!(
                scene
                    .restore_sound_state_bytes(&bytes[..length], 8192)
                    .is_err(),
                "truncated state length={length}",
            );
            assert_eq!(scene.sound_state_bytes(8192).unwrap(), before);
        }
        assert!(
            scene
                .restore_sound_state_bytes(&bytes, bytes.len() - 1)
                .is_err()
        );
        assert_eq!(scene.sound_state_bytes(8192).unwrap(), before);
        scene.restore_sound_state_bytes(&bytes, 8192).unwrap();
        assert_eq!(scene.sound_requests(), &[cue]);
        assert_eq!((scene.time().frames(), scene.time().fps()), clock);
        assert!(!scene.can_resume_wait_checkpoint());
        assert!(scene.can_resume_wait_checkpoint_with_sound_state());
        scene.restore_sound_state(SoundState::default());
        assert!(scene.sound_requests().is_empty());
        assert!(scene.can_resume_wait_checkpoint());
        assert_eq!((scene.time().frames(), scene.time().fps()), clock);
        let inbox = scene.event_inbox();
        assert!(!scene.can_resume_wait_checkpoint_with_sound_state());
        drop(inbox);
        assert!(scene.can_resume_wait_checkpoint_with_sound_state());
    }

    #[test]
    fn sound_state_refuses_invalid_metadata_inside_valid_checksummed_envelopes() {
        for (index, fps, offset, placement, gain, background) in [
            (1, 60, 0, 0, 0, 0),
            (0, 0, 0, 0, 0, 0),
            (0, 60, f64::NAN.to_bits(), 0, 0, 0),
            (0, 60, f64::INFINITY.to_bits(), 0, 0, 0),
            (0, 60, (-0.0_f64).to_bits(), 0, 0, 0),
            (0, 60, 0, 3, 0, 0),
            (0, 60, 0, 0, 2, 0),
            (0, 60, 0, 0, 0, 2),
        ] {
            let mut writer = sound_writer();
            writer
                .put_u32(1)
                .put_u8(0)
                .put_u8(0)
                .put_str("x")
                .put_u32(1)
                .put_u32(index)
                .put_i64(3)
                .put_u32(fps)
                // Write the raw bits so the reader sees noncanonical/nonfinite
                // floats under a valid outer checksum.
                .put_u64(offset)
                .put_u8(placement)
                .put_u8(gain)
                .put_u8(background);
            assert!(SoundState::from_bytes(&writer.finish().unwrap(), 8192).is_err());
        }

        for (paths, indices) in [
            (["x", "x"], vec![0, 1]),
            (["x", "y"], vec![0]),
            (["x", "y"], vec![1, 0]),
        ] {
            let mut writer = sound_writer();
            writer.put_u32(2);
            for path in paths {
                writer.put_u8(0).put_u8(0).put_str(path);
            }
            writer.put_u32(indices.len() as u32);
            for index in indices {
                put_minimal_cue(&mut writer, index);
            }
            assert!(SoundState::from_bytes(&writer.finish().unwrap(), 8192).is_err());
        }
        let empty = SoundState::default().to_bytes(64).unwrap();
        assert_eq!(empty.len(), 64);
        assert!(SoundState::from_bytes(&empty, 64).unwrap().is_empty());
        assert!(SoundState::default().to_bytes(63).is_err());
    }

    #[test]
    fn sound_state_verifies_embedded_digest_and_canonical_finite_pcm() {
        let data = SoundData::from_audio(&pcm(), SoundDataLimits::default()).unwrap();
        let compact = encode_wav(2, 44_100, SampleFormat::S16, &pcm().samples);
        let mut nonfinite = data.bytes().to_vec();
        nonfinite[44..48].copy_from_slice(&f32::NAN.to_bits().to_le_bytes());
        for (wav, digest) in [
            (data.bytes().as_ref(), sha256(b"wrong source identity")),
            (compact.as_slice(), sha256(&compact)),
            (nonfinite.as_slice(), sha256(&nonfinite)),
        ] {
            let mut writer = sound_writer();
            writer
                .put_u32(1)
                .put_u8(1)
                .put_digest(&digest)
                .put_bytes(wav)
                .put_u32(1);
            put_minimal_cue(&mut writer, 0);
            assert!(SoundState::from_bytes(&writer.finish().unwrap(), 8192).is_err());
        }
    }

    #[test]
    fn sound_state_bounds_path_expansion_even_when_the_wire_source_is_shared() {
        let cue = checkpoint_cue(SoundSource::File("a".repeat(128).into()), 0);
        let requests = vec![cue; 32];
        let bytes = SoundState::encode_requests(&requests, 8192).unwrap();
        assert!(bytes.len() < 2048);
        for result in [
            SoundState::encode_requests(&requests, 2048).map(|_| ()),
            SoundState::from_bytes(&bytes, 2048).map(|_| ()),
        ] {
            assert!(matches!(
                result,
                Err(SoundStateError::LimitExceeded {
                    resource: "decoded file path bytes",
                    limit: 2048,
                })
            ));
        }
        assert_eq!(
            SoundState::from_bytes(&bytes, 8192).unwrap().requests(),
            requests,
        );
    }

    #[cfg(unix)]
    #[test]
    fn sound_state_preserves_non_utf8_unix_paths_without_opening_them() {
        use std::os::unix::ffi::{OsStrExt, OsStringExt};

        let spelling = b"missing/voice-\xff.wav";
        let path = PathBuf::from(OsString::from_vec(spelling.to_vec()));
        let bytes =
            SoundState::encode_requests(&[checkpoint_cue(SoundSource::File(path), 0)], 8192)
                .unwrap();
        let decoded = SoundState::from_bytes(&bytes, 8192).unwrap();
        let SoundSource::File(path) = &decoded.requests()[0].source else {
            panic!("a native path became a memory source");
        };
        assert_eq!(path.as_os_str().as_bytes(), spelling);
        assert_eq!(decoded.to_bytes(8192).unwrap(), bytes);
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
