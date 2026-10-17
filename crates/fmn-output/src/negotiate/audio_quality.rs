//! Explicit compressed-soundtrack controls. Native PCM mixing is unchanged.

use super::NegotiationError;

/// Opt-in AAC encoding policy for the second-stage soundtrack mux.
///
/// `Default` preserves ffmpeg's existing AAC defaults. This is independent of
/// video quality: muxing always copies the video stream and never re-encodes it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct AudioQuality {
    /// Target AAC bitrate in bits per second. This is an encoder target, not
    /// an output-size guarantee. Zero is refused; no shell/argv text is accepted.
    pub bitrate: Option<u32>,
}

impl AudioQuality {
    /// Whether the installed AAC encoder's defaults are requested.
    #[must_use]
    pub const fn is_default(self) -> bool {
        self.bitrate.is_none()
    }

    /// Validate before any encoder process or output generation is started.
    ///
    /// # Errors
    /// A zero target is not an AAC bitrate.
    pub const fn validate(self) -> Result<(), NegotiationError> {
        if matches!(self.bitrate, Some(0)) {
            Err(NegotiationError("AAC bitrate must be nonzero"))
        } else {
            Ok(())
        }
    }
}
