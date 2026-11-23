//! Observe the authored scene clock for composition-only WAV export.

use fmn::animation::FramePacket;
use fmn::rendering::{RenderError, SoundtrackTimeline};
use fmn_scene::{CaptureReason, IntegrationError, LifecycleEvent, Scene, SceneSink};

use crate::{CliError, native_render_error};

/// Skipped time advances the scene without advancing its exported soundtrack.
/// This sink needs only lifecycle metadata, so it never freezes or renders
/// geometry. Errors stay latched even if authored code catches a sink failure.
#[derive(Default)]
pub(super) struct SoundtrackCapture {
    timeline: SoundtrackTimeline,
    failure: Option<RenderError>,
}

impl SoundtrackCapture {
    pub(super) fn finish(mut self, scene: &Scene) -> Result<SoundtrackTimeline, CliError> {
        if let Some(error) = self.failure {
            return Err(native_render_error(error));
        }
        self.timeline
            .observe(scene.time().frames(), scene.is_skipping())
            .map_err(native_render_error)?;
        Ok(self.timeline)
    }
}

impl SceneSink for SoundtrackCapture {
    fn event(&mut self, event: LifecycleEvent) -> Result<(), IntegrationError> {
        if self.failure.is_none() {
            self.failure = self
                .timeline
                .observe(event.time.frames(), event.skipping)
                .err();
        }
        match &self.failure {
            Some(error) => Err(IntegrationError::new("wav-timeline", error.to_string())),
            None => Ok(()),
        }
    }

    fn capture_state_required(&self) -> bool {
        false
    }

    fn capture(&mut self, _: CaptureReason, _: FramePacket) -> Result<(), IntegrationError> {
        Ok(())
    }

    fn accepts_pure_segments(&self) -> bool {
        self.failure.is_none()
    }

    fn capture_pure_segment(
        &mut self,
        _: &fmn::animation::pure_frame::PureSegment,
    ) -> Result<(), IntegrationError> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{WavComposition, WavDecodeContext, WavEncoding, WavSampleFormat};
    use fmn::prelude::{SceneConstruct, Stage};
    use fmn::rendering::SoundtrackDecoderOptions;
    use fmn_codec::{SampleFormat, encode_wav};
    use fmn_platform::fs::{FileSystem, VirtualFs};
    use fmn_scene::{RuntimeConfig, SoundSource};
    use std::path::Path;

    struct SelectedNarration;

    impl SceneConstruct for SelectedNarration {
        fn construct(&mut self, stage: &mut Stage<'_>) -> fmn::Result<()> {
            // The animation-range prefix advances one second without output.
            stage.wait(1.0)?;
            stage.wait(0.25)?;
            stage
                .scene_mut()
                .add_sound("/voice.wav", -0.125, None, None)?;
            stage.scene_mut().add_sound_source_at(
                SoundSource::File("/voice.wav".into()),
                1.375,
                None,
                None,
            )?;
            stage.wait(0.25)?;
            // An authored skipped interval in the middle has the same mapping.
            stage.scene_mut().force_skipping();
            stage.wait(0.5)?;
            stage.scene_mut().revert_to_original_skipping_status();
            stage.wait(0.25)?;
            stage.scene_mut().add_sound("/voice.wav", 0.0, None, None)?;
            stage.wait(0.25)?;
            Ok(())
        }
    }

    #[test]
    fn cli_wav_rebases_ranges_offsets_and_absolute_cues_at_every_worker_count() {
        let fs = VirtualFs::new();
        let mut voice = vec![0.25_f32; 12_000];
        voice[6_000..].fill(-0.5);
        fs.insert(
            "/voice.wav",
            encode_wav(1, 48_000, SampleFormat::S16, &voice),
        );
        // One second of stereo output, with independently placed quarter-second
        // cues at 1/8, 3/8 and 3/4 seconds. Neither skipped interval contributes
        // silence and relative offsets are applied exactly once.
        let mut expected = vec![0.0_f32; 96_000];
        for start in [6_000, 18_000, 36_000] {
            for (index, &sample) in voice.iter().enumerate() {
                expected[2 * (start + index)..2 * (start + index + 1)].fill(sample);
            }
        }
        let expected = encode_wav(2, 48_000, SampleFormat::S16, &expected);
        for workers in [1, 4, 16] {
            let mut capture = SoundtrackCapture::default();
            let scene = fmn::run_scene(
                &mut SelectedNarration,
                RuntimeConfig {
                    fps: 8,
                    start_at_play: Some(1),
                    ..RuntimeConfig::default()
                },
                0,
                &mut capture,
            )
            .unwrap()
            .into_scene();
            assert_eq!(scene.time().frames(), 20);
            let timeline = capture.finish(&scene).unwrap();
            assert_eq!(timeline.output_frame(20).unwrap(), 8);
            let options = SoundtrackDecoderOptions::default();
            let mut decode = WavDecodeContext {
                decoder: options
                    .decoder(&fmn_config::Config::default(), None)
                    .unwrap(),
                options,
            };
            let destination = format!("/selected-{workers}.wav");
            let (_, assets) = crate::publish_scene_soundtrack(
                &fs,
                Path::new(&destination),
                1 << 20,
                &scene,
                workers,
                WavComposition {
                    encoding: WavEncoding {
                        format: WavSampleFormat::S16,
                        dither_seed: None,
                    },
                    decode: &mut decode,
                    timeline: &timeline,
                },
            )
            .unwrap();
            assert_eq!(assets.len(), 3);
            assert_eq!(fs.read(Path::new(&destination)).unwrap(), expected);
        }
    }
}
