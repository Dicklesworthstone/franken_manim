//! A single validated source generation shared by picture and audio preview.

use fmn_scene::{AssetRead, TimelineBundle};

use crate::{ServiceError, StudioAudioInfo, StudioAudioTrack, WorkerErrorCode};

/// Immutable audio and clock metadata for the recorded-mode Studio host.
///
/// Construct through [`Self::decode`] so the original FMTL digest binds both
/// streams. The host serves PCM only while its session asset verifier still
/// admits that source. New source generations require a new binding; this type
/// does not turn live scene callbacks into an audio producer.
#[derive(Clone, Debug)]
pub struct StudioRecordedAudio {
    pub(crate) source: AssetRead,
    pub(crate) track: StudioAudioTrack,
    pub(crate) fps: u32,
    pub(crate) picture_frames: u32,
}

impl StudioRecordedAudio {
    /// Decode once, returning pictures for the isolated worker and optional
    /// audio for the stable host. No source file is opened by this API.
    ///
    /// `path` is the exact virtual source identity used in the worker's input
    /// closure, not a request-controlled file path. Both the supervisor and
    /// worker must use the same validated recording generation.
    ///
    /// # Errors
    /// Refuses invalid source identities and preserves decoder diagnostics.
    pub fn decode(
        path: &str,
        bytes: &[u8],
    ) -> Result<(TimelineBundle, Option<Self>), ServiceError> {
        if path.is_empty() || path.len() > 4096 || path.contains('\0') {
            return Err(ServiceError::new(
                WorkerErrorCode::InvalidRequest,
                "invalid recorded Studio source identity",
            ));
        }
        let (bundle, audio) = StudioAudioTrack::read_recording(bytes).map_err(|error| {
            ServiceError::new(WorkerErrorCode::ExecutionFailed, error.to_string())
        })?;
        let audio = audio.map(|track| Self {
            source: AssetRead {
                path: path.to_owned(),
                digest: track.info().source,
            },
            track,
            fps: bundle.fps(),
            picture_frames: bundle.frame_count(),
        });
        Ok((bundle, audio))
    }

    /// Complete recording identity to include in the worker's source reads.
    #[must_use]
    pub fn source(&self) -> &AssetRead {
        &self.source
    }

    /// Stored sample clock, channels, duration and complete recording digest.
    #[must_use]
    pub fn info(&self) -> StudioAudioInfo {
        self.track.info()
    }

    /// Original picture frame rate, independent of the audio sample clock.
    #[must_use]
    pub const fn fps(&self) -> u32 {
        self.fps
    }

    /// Original picture-frame count; audio may have a longer tail.
    #[must_use]
    pub const fn picture_frames(&self) -> u32 {
        self.picture_frames
    }
}
