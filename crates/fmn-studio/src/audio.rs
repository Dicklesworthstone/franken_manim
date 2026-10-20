//! Bounded, source-bound PCM access for recorded Studio narration.
//!
//! A preview host keeps one validated soundtrack, not an encoded copy or a
//! history of playback chunks. Sample offsets are on the recording's original
//! clock; seeking does not execute scene code, decode assets, mix or resample.
//! The browser/device is responsible for physical playback. This transport does
//! not promise bit-identical sound from different audio devices.

use std::fmt;
use std::sync::Arc;

use fmn_hash::{Digest, sha256};
use fmn_scene::timeline_bundle::{BundleReadError, BundleSoundtrack, TimelineBundle};

/// Maximum interleaved sample frames in one preview response (512 KiB stereo).
pub const MAX_AUDIO_CHUNK_FRAMES: u32 = 65_536;
/// Maximum raw Float32 payload in one preview response.
pub const MAX_AUDIO_CHUNK_BYTES: usize = MAX_AUDIO_CHUNK_FRAMES as usize * 2 * 4;

/// Stable metadata for one immutable soundtrack generation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StudioAudioInfo {
    /// Digest of the complete source FMTL, including pictures and camera track.
    pub source: Digest,
    /// Recorded sample clock, not a requested device rate.
    pub sample_rate: u32,
    /// Recorded interleaving: one mono channel or left/right stereo.
    pub channels: u16,
    /// Stored duration, including any tail beyond the last picture.
    pub sample_frames: u64,
}

/// Immutable recorded narration, cheaply shareable between independent clients.
#[derive(Clone, Debug)]
pub struct StudioAudioTrack {
    source: Digest,
    soundtrack: Arc<BundleSoundtrack>,
}

/// One independently addressed, bounded audio response.
///
/// The bytes are raw IEEE-754 Float32, little-endian and interleaved. There is
/// no WAV header, lossy conversion, dither or implicit silence after EOF.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StudioAudioChunk {
    info: StudioAudioInfo,
    start: u64,
    frames: u32,
    bytes: Vec<u8>,
    digest: Digest,
}

/// Refusal of a recorded preview request, before payload allocation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StudioAudioError {
    /// The client is requesting a previous/different recording generation.
    SourceChanged,
    /// The request must contain 1..=MAX_AUDIO_CHUNK_FRAMES sample frames.
    ChunkSize,
    /// A sample cursor beyond EOF is not a valid position in this recording.
    Position,
    /// The bounded response could not be allocated.
    Allocation,
}

impl fmt::Display for StudioAudioError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::SourceChanged => {
                "Studio audio source changed; refresh its metadata before playback"
            }
            Self::ChunkSize => "Studio audio requests require 1..=65536 sample frames",
            Self::Position => "Studio audio sample cursor is beyond the recording",
            Self::Allocation => "could not allocate the bounded Studio audio response",
        })
    }
}

impl std::error::Error for StudioAudioError {}

impl StudioAudioTrack {
    /// Retain an already-validated native soundtrack without copying its PCM.
    ///
    /// The caller owns the association between `source` and `soundtrack`. Use
    /// [`Self::read_recording`] to derive that identity from the actual bytes.
    #[must_use]
    pub fn new(source: Digest, soundtrack: BundleSoundtrack) -> Self {
        Self {
            source,
            soundtrack: Arc::new(soundtrack),
        }
    }

    /// Read through the production decoder and explicitly separate pictures
    /// from optional narration. The original source digest binds both streams.
    ///
    /// The returned picture bundle retains its original frame indices, camera
    /// samples and reconstruction proof, and can enter the ordinary Studio
    /// worker. No playback command or rasterization runs here.
    ///
    /// # Errors
    /// Preserves the production reader's schema, integrity and resource errors.
    pub fn read_recording(
        bytes: &[u8],
    ) -> Result<(TimelineBundle, Option<Self>), BundleReadError> {
        let mut bundle = TimelineBundle::from_bytes(bytes)?;
        let audio = bundle
            .take_soundtrack()
            .map(|soundtrack| Self::new(sha256(bytes), soundtrack));
        Ok((bundle, audio))
    }

    /// Metadata is constant-time and does not copy or hash the entire PCM.
    #[must_use]
    pub fn info(&self) -> StudioAudioInfo {
        StudioAudioInfo {
            source: self.source,
            sample_rate: self.soundtrack.sample_rate(),
            channels: self.soundtrack.channels(),
            sample_frames: self.soundtrack.sample_frames(),
        }
    }

    /// Read at most `frames` original sample frames starting at `start`.
    ///
    /// Repeated, backward, out-of-order and concurrent requests are independent
    /// and never change a playback cursor or journal. The caller must present
    /// the source identity it obtained with the metadata, so reload races fail
    /// instead of serving different narration under an old clock.
    ///
    /// A final chunk is shortened at EOF. Exactly EOF returns an empty terminal
    /// chunk; a cursor beyond EOF refuses. Memory and work depend on this chunk,
    /// never the elapsed prefix or the complete soundtrack duration.
    ///
    /// # Errors
    /// Wrong source, zero/oversized chunk, cursor beyond EOF, or allocation.
    pub fn chunk(
        &self,
        expected_source: Digest,
        start: u64,
        frames: u32,
    ) -> Result<StudioAudioChunk, StudioAudioError> {
        if expected_source != self.source {
            return Err(StudioAudioError::SourceChanged);
        }
        if frames == 0 || frames > MAX_AUDIO_CHUNK_FRAMES {
            return Err(StudioAudioError::ChunkSize);
        }
        let info = self.info();
        let remaining = info
            .sample_frames
            .checked_sub(start)
            .ok_or(StudioAudioError::Position)?;
        let frames = u32::try_from(remaining.min(u64::from(frames)))
            .map_err(|_| StudioAudioError::ChunkSize)?;
        let channels = usize::from(info.channels);
        let first = usize::try_from(start)
            .ok()
            .and_then(|offset| offset.checked_mul(channels))
            .ok_or(StudioAudioError::Position)?;
        let count = usize::try_from(frames)
            .ok()
            .and_then(|count| count.checked_mul(channels))
            .ok_or(StudioAudioError::ChunkSize)?;
        let samples = self
            .soundtrack
            .samples()
            .get(first..first.checked_add(count).ok_or(StudioAudioError::Position)?)
            .ok_or(StudioAudioError::Position)?;
        let byte_count = count.checked_mul(4).ok_or(StudioAudioError::ChunkSize)?;
        if byte_count > MAX_AUDIO_CHUNK_BYTES {
            return Err(StudioAudioError::ChunkSize);
        }
        let mut bytes = Vec::new();
        bytes
            .try_reserve_exact(byte_count)
            .map_err(|_| StudioAudioError::Allocation)?;
        for sample in samples {
            bytes.extend_from_slice(&sample.to_le_bytes());
        }
        let digest = sha256(&bytes);
        Ok(StudioAudioChunk {
            info,
            start,
            frames,
            bytes,
            digest,
        })
    }
}

impl StudioAudioChunk {
    /// Recording metadata associated with this response, including its identity.
    #[must_use]
    pub const fn info(&self) -> StudioAudioInfo {
        self.info
    }

    /// Original sample cursor (not a picture frame index).
    #[must_use]
    pub const fn start(&self) -> u64 {
        self.start
    }

    /// Actual sample frames, possibly shortened at EOF.
    #[must_use]
    pub const fn frames(&self) -> u32 {
        self.frames
    }

    /// Next exact sample cursor, independent of request or response timing.
    #[must_use]
    pub fn next(&self) -> u64 {
        self.start + u64::from(self.frames)
    }

    /// This chunk reaches the end of the stored soundtrack, including its tail.
    #[must_use]
    pub fn is_terminal(&self) -> bool {
        self.next() == self.info.sample_frames
    }

    /// Borrow the bounded little-endian interleaved Float32 payload.
    #[must_use]
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    /// Raw payload integrity digest, distinct from the recording identity.
    #[must_use]
    pub const fn digest(&self) -> Digest {
        self.digest
    }

    /// Transfer the existing bounded buffer to a transport without cloning it.
    #[must_use]
    pub fn into_bytes(self) -> Vec<u8> {
        self.bytes
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> StudioAudioTrack {
        let samples = (0_u16..1000)
            .flat_map(|n| [f32::from(n) / 1024.0, -f32::from(n) / 2048.0])
            .collect();
        StudioAudioTrack::new(
            sha256(b"complete recording"),
            BundleSoundtrack::new(44_100, 2, samples).unwrap(),
        )
    }

    #[test]
    fn chunks_partition_the_original_bits_and_keep_channel_order() {
        let track = fixture();
        let expected: Vec<_> = track
            .soundtrack
            .samples()
            .iter()
            .flat_map(|sample| sample.to_le_bytes())
            .collect();
        for size in [1, 7, 127, MAX_AUDIO_CHUNK_FRAMES] {
            let mut cursor = 0;
            let mut joined = Vec::new();
            loop {
                let chunk = track.chunk(track.info().source, cursor, size).unwrap();
                assert_eq!(chunk.info(), track.info());
                assert_eq!(chunk.start(), cursor);
                assert_eq!(chunk.bytes().len(), chunk.frames() as usize * 2 * 4);
                assert_eq!(chunk.digest(), sha256(chunk.bytes()));
                assert!(chunk.bytes().len() <= MAX_AUDIO_CHUNK_BYTES);
                cursor = chunk.next();
                let terminal = chunk.is_terminal();
                joined.extend(chunk.into_bytes());
                if terminal {
                    break;
                }
            }
            assert_eq!(cursor, 1000);
            assert_eq!(joined, expected);
        }
    }

    #[test]
    fn seeks_and_concurrent_clients_have_no_shared_cursor() {
        let track = fixture();
        let expected = track.chunk(track.info().source, 731, 73).unwrap();
        std::thread::scope(|scope| {
            let jobs: Vec<_> = (0..8)
                .map(|_| {
                    let track = &track;
                    scope.spawn(move || {
                        for offset in [999, 0, 231, 6] {
                            track.chunk(track.info().source, offset, 29).unwrap();
                        }
                        track.chunk(track.info().source, 731, 73).unwrap()
                    })
                })
                .collect();
            for job in jobs {
                assert_eq!(job.join().unwrap(), expected);
            }
        });
        assert_eq!(track.info().sample_frames, 1000);
    }

    #[test]
    fn stale_sources_and_invalid_requests_cannot_select_other_audio() {
        let track = fixture();
        let source = track.info().source;
        assert_eq!(
            track.chunk(sha256(b"old recording"), 0, 1),
            Err(StudioAudioError::SourceChanged)
        );
        for frames in [0, MAX_AUDIO_CHUNK_FRAMES + 1, u32::MAX] {
            assert_eq!(
                track.chunk(source, 0, frames),
                Err(StudioAudioError::ChunkSize)
            );
        }
        for start in [1001, u64::MAX] {
            assert_eq!(track.chunk(source, start, 1), Err(StudioAudioError::Position));
        }
        assert_eq!(track.chunk(source, 0, 1).unwrap().frames(), 1);
    }

    #[test]
    fn final_and_empty_chunks_do_not_invent_silence_or_extend_the_timeline() {
        let track = fixture();
        let chunk = track.chunk(track.info().source, 997, 100).unwrap();
        assert_eq!(chunk.frames(), 3);
        assert_eq!(chunk.next(), 1000);
        assert!(chunk.is_terminal());
        let eof = track.chunk(track.info().source, 1000, 1).unwrap();
        assert_eq!(eof.frames(), 0);
        assert!(eof.bytes().is_empty());
        assert!(eof.is_terminal());
        let empty = StudioAudioTrack::new(
            sha256(b"empty"),
            BundleSoundtrack::new(48_000, 1, Vec::new()).unwrap(),
        );
        assert!(empty.chunk(empty.info().source, 0, 32).unwrap().is_terminal());
    }

    #[test]
    fn mono_and_stereo_maximum_responses_respect_the_wire_ceiling() {
        for channels in [1, 2] {
            let frames = MAX_AUDIO_CHUNK_FRAMES as usize;
            let track = StudioAudioTrack::new(
                sha256(b"large"),
                BundleSoundtrack::new(
                    192_000,
                    channels,
                    vec![0.125; (frames + 1) * usize::from(channels)],
                )
                .unwrap(),
            );
            let chunk = track
                .chunk(track.info().source, 1, MAX_AUDIO_CHUNK_FRAMES)
                .unwrap();
            assert_eq!(chunk.bytes().len(), frames * usize::from(channels) * 4);
            assert!(chunk.bytes().len() <= MAX_AUDIO_CHUNK_BYTES);
            assert!(chunk.is_terminal());
        }
    }

    #[test]
    fn validated_recording_split_preserves_pictures_and_the_audio_tail() {
        let mut stage = fmn_mobject::Stage::new();
        let mut timeline = fmn_anim::Timeline::new(3).unwrap();
        timeline.wait(1.0).unwrap();
        let pictures = fmn_scene::export_timeline_bundle(
            timeline,
            &mut stage,
            &fmn_core::rng::RngRoot::from_seed(0),
        )
        .unwrap();
        let pcm = BundleSoundtrack::new(10, 1, vec![0.25; 14]).unwrap();
        let bytes = fmn_scene::timeline_bundle::attach_bundle_soundtrack(
            &pictures,
            &pcm,
            fmn_scene::DEFAULT_MAX_BUNDLE_BYTES,
        )
        .unwrap();
        let (bundle, audio) = StudioAudioTrack::read_recording(&bytes).unwrap();
        assert_eq!(bundle.frame_count(), 3);
        assert_eq!(bundle.fps(), 3);
        assert!(!bundle.has_soundtrack());
        bundle
            .into_shared()
            .unwrap()
            .frame_job(2)
            .unwrap()
            .materialize()
            .unwrap();
        let audio = audio.unwrap();
        assert_eq!(audio.info().source, sha256(&bytes));
        assert_eq!(audio.info().sample_frames, 14);
        assert_eq!(audio.chunk(sha256(&bytes), 10, 10).unwrap().frames(), 4);
        let (_, absent) = StudioAudioTrack::read_recording(&pictures).unwrap();
        assert!(absent.is_none());
        let mut corrupt = bytes;
        corrupt[0] ^= 1;
        assert!(StudioAudioTrack::read_recording(&corrupt).is_err());
    }
}
