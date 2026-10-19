//! Explicit transfer of final bundle PCM to a browser audio coordinator.
//! The picture-only loader stays fail-closed. This loader accepts the same
//! codec, but rendering stays blocked until the host takes responsibility.

use super::{FmnPlayer, PlayerCore, PlayerError};
use fmn_scene::TimelineBundle;
use fmn_scene::timeline_bundle::BundleSoundtrack;
use wasm_bindgen::{JsError, prelude::wasm_bindgen};

impl PlayerCore {
    fn load_with_audio(bytes: &[u8]) -> Result<Self, PlayerError> {
        Ok(Self {
            bundle: TimelineBundle::from_bytes(bytes)?,
            width: 0,
            height: 0,
            cursor: 0,
        })
    }
}

#[wasm_bindgen]
impl FmnPlayer {
    /// Load pictures and optional PCM for a host that explicitly handles audio.
    /// Call `take_soundtrack` before rendering sound-bearing input, then copy
    /// its channels to a host audio buffer and schedule from movie time zero.
    /// No device, codec, source callback or filesystem is accessed here.
    /// Ordinary `from_bundle` still refuses sound-bearing input at load.
    ///
    /// # Errors
    /// The production decoder's schema, engine, snapshot and PCM refusals.
    pub fn from_bundle_with_audio(bytes: &[u8]) -> Result<FmnPlayer, JsError> {
        Ok(FmnPlayer {
            core: PlayerCore::load_with_audio(bytes)?,
        })
    }

    /// Whether the host still needs to assume responsibility for attached PCM.
    #[wasm_bindgen(getter)]
    pub fn has_soundtrack(&self) -> bool {
        self.core.bundle.has_soundtrack()
    }

    /// Transfer PCM exactly once without copying it, authorizing the existing
    /// picture renderer. Inspecting metadata alone does not authorize playback.
    pub fn take_soundtrack(&mut self) -> Option<FmnSoundtrack> {
        self.core
            .bundle
            .take_soundtrack()
            .map(|pcm| FmnSoundtrack { pcm })
    }
}

/// Immutable normalized Float32 PCM, already mixed during native export.
/// Copy channels into caller buffers, then call the generated `free()` to
/// release Wasm storage. Output-device resampling belongs to the host and
/// does not alter the stored waveform or the bundle's picture clock.
#[wasm_bindgen]
pub struct FmnSoundtrack {
    pcm: BundleSoundtrack,
}

#[derive(Debug, PartialEq, Eq)]
enum CopyError {
    Channel,
    Range,
}
impl std::fmt::Display for CopyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Channel => "soundtrack channel is outside the mono/stereo layout",
            Self::Range => "soundtrack copy range is outside the stored sample frames",
        })
    }
}
impl std::error::Error for CopyError {}

impl FmnSoundtrack {
    fn copy_channel(&self, channel: u32, start: u32, dst: &mut [f32]) -> Result<(), CopyError> {
        if channel >= u32::from(self.pcm.channels()) {
            return Err(CopyError::Channel);
        }
        let start = usize::try_from(start).map_err(|_| CopyError::Range)?;
        let channels = usize::from(self.pcm.channels());
        let total = self.pcm.samples().len() / channels;
        if start > total || dst.len() > total - start {
            return Err(CopyError::Range);
        }
        // All validation precedes mutation. Products fit within source storage.
        for (index, sample) in dst.iter_mut().enumerate() {
            *sample = self.pcm.samples()[(start + index) * channels + channel as usize];
        }
        Ok(())
    }
}

#[wasm_bindgen]
impl FmnSoundtrack {
    /// Original sample frames per second, not picture FPS or device rate.
    #[wasm_bindgen(getter)]
    pub fn sample_rate(&self) -> u32 {
        self.pcm.sample_rate()
    }
    /// One for mono, two for left/right stereo.
    #[wasm_bindgen(getter)]
    pub fn channels(&self) -> u32 {
        u32::from(self.pcm.channels())
    }
    /// Complete sample frames; the codec's 256 MiB ceiling keeps this below u32.
    #[wasm_bindgen(getter)]
    pub fn sample_frames(&self) -> u32 {
        self.pcm.sample_frames() as u32
    }
    /// Stored audio duration, including tails beyond the picture timeline.
    #[wasm_bindgen(getter)]
    pub fn duration_seconds(&self) -> f64 {
        f64::from(self.sample_frames()) / f64::from(self.sample_rate())
    }
    /// Copy one channel window into caller storage, suitable for AudioBuffer.
    /// Offsets and lengths count sample frames, not interleaved sample indices.
    /// Empty windows at the exact end are legal. The bridge marshals the slice;
    /// this is bounded copying, not a zero-copy browser view.
    ///
    /// # Errors
    /// Invalid channel or out-of-range window; the destination is unchanged.
    pub fn copy_channel_into(
        &self,
        channel: u32,
        start_frame: u32,
        dst: &mut [f32],
    ) -> Result<(), JsError> {
        Ok(self.copy_channel(channel, start_frame, dst)?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fmn_mobject::Stage;
    use fmn_render::{Camera, CameraConfig};
    use fmn_scene::{BundleExportLimits, recording::SceneBundleRecorder};

    fn visual(with_camera: bool) -> Vec<u8> {
        let limits = BundleExportLimits::default();
        if with_camera {
            let mut camera = Camera::new(CameraConfig {
                fps: 8,
                resolution: (16, 8),
                ..Default::default()
            })
            .unwrap();
            camera
                .set_background(fmn_core::color::Srgb::from_rgb8(255, 0, 0).to_linear(1.0))
                .unwrap();
            let mut recorder = SceneBundleRecorder::new_render_only_with_camera(8, limits).unwrap();
            recorder
                .capture_terminal_still_with_camera(&Stage::new(), &camera)
                .unwrap();
            recorder.finish().unwrap().bytes
        } else {
            let mut recorder = SceneBundleRecorder::new_render_only(8, limits).unwrap();
            recorder.capture_terminal_still(&Stage::new()).unwrap();
            recorder.finish().unwrap().bytes
        }
    }
    fn track(channels: u16) -> FmnSoundtrack {
        FmnSoundtrack {
            pcm: BundleSoundtrack::new(8, channels, vec![0.125, -0.25, 0.5, -1.0]).unwrap(),
        }
    }

    #[test]
    fn channel_windows_preserve_exact_pcm_and_independent_storage() {
        let audio = track(2);
        assert_eq!(
            (audio.channels(), audio.sample_rate(), audio.sample_frames()),
            (2, 8, 2)
        );
        assert_eq!(audio.duration_seconds(), 0.25);
        let mut left = [9.0; 2];
        let mut right = [9.0; 2];
        audio.copy_channel(0, 0, &mut left).unwrap();
        audio.copy_channel(1, 0, &mut right).unwrap();
        assert_eq!(left, [0.125, 0.5]);
        assert_eq!(right, [-0.25, -1.0]);
        audio.copy_channel(1, 1, &mut left[..1]).unwrap();
        assert_eq!(left, [-1.0, 0.5]);
        assert_eq!(audio.pcm.samples(), [0.125, -0.25, 0.5, -1.0]);
        let mut mono = [0.0; 3];
        track(1).copy_channel(0, 1, &mut mono).unwrap();
        assert_eq!(mono, [-0.25, 0.5, -1.0]);
    }
    #[test]
    fn invalid_channel_or_window_never_touches_the_destination() {
        let audio = track(2);
        for (channel, start, error) in [
            (2, 0, CopyError::Channel),
            (u32::MAX, 0, CopyError::Channel),
            (0, 1, CopyError::Range),
            (0, u32::MAX, CopyError::Range),
        ] {
            let mut dst = [42.0; 2];
            assert_eq!(audio.copy_channel(channel, start, &mut dst), Err(error));
            assert_eq!(dst, [42.0; 2]);
        }
        assert!(audio.copy_channel(1, 2, &mut []).is_ok());
        assert_eq!(audio.copy_channel(0, 3, &mut []), Err(CopyError::Range));
        let empty = FmnSoundtrack {
            pcm: BundleSoundtrack::new(8, 1, vec![]).unwrap(),
        };
        assert_eq!(empty.sample_frames(), 0);
        assert_eq!(empty.duration_seconds(), 0.0);
        assert!(empty.copy_channel(0, 0, &mut []).is_ok());
    }
    #[test]
    fn explicit_transfer_unlocks_the_same_camera_and_planar_pictures() {
        for camera in [false, true] {
            let pictures = visual(camera);
            let bytes = fmn_scene::timeline_bundle::attach_bundle_soundtrack(
                &pictures,
                &track(2).pcm,
                usize::MAX,
            )
            .unwrap();
            assert!(PlayerCore::load(&bytes).is_err());
            let mut core = PlayerCore::load_with_audio(&bytes).unwrap();
            core.set_viewport(16, 8).unwrap();
            assert!(core.render_index(0).is_err());
            let mut player = FmnPlayer { core };
            assert!(player.has_soundtrack());
            let pcm = player.take_soundtrack().unwrap();
            assert_eq!(pcm.pcm, track(2).pcm);
            assert!(!player.has_soundtrack());
            assert!(player.take_soundtrack().is_none());
            let mut expected = PlayerCore::load(&pictures).unwrap();
            expected.set_viewport(16, 8).unwrap();
            let pixels = player.core.render_index(0).unwrap();
            assert_eq!(pixels, expected.render_index(0).unwrap());
            if camera {
                assert!(
                    pixels
                        .as_chunks::<4>()
                        .0
                        .iter()
                        .all(|p| *p == [255, 0, 0, 255])
                );
            }
            assert!(
                !FmnPlayer::from_bundle_with_audio(&pictures)
                    .unwrap()
                    .has_soundtrack()
            );
        }
    }
}
