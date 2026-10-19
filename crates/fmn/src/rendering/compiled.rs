//! Native output for compiled timelines, with reconstruction on render teams.

use fmn_anim::RationalFrameClock;
use fmn_platform::fs::{FileSystem, StdFs};
use fmn_runtime::{FrameStreamError, PipelineError, PipelineStats};
use fmn_scene::SceneRunReport;
use fmn_scene::timeline_bundle::{BundleSoundtrack, TimelineBundle};
use std::ops::Range;
use std::sync::Arc;

use super::{RenderError, RenderFormat, RenderOptions, RenderReport, RenderSink};

/// Render a validated FMTL artifact through the native bounded CPU pipeline.
///
/// The bundle's clock and export-time reconstruction proof remain authoritative.
/// Pure segments reconstruct from their endpoints on independent render teams;
/// stateful segments use their recorded captures and never replay callbacks.
/// Output is strictly frame-index ordered. This is compiled replay, not a claim
/// that arbitrary live native scenes have become frame-parallel.
///
/// `options.config.camera.fps` must equal the artifact's frame rate. No frame
/// resampling or alpha-zero insertion is performed. [`RenderFormat::Png`]
/// publishes only the final recorded sample, retaining the bundle's full
/// scene time and play count. It cannot reconstruct unrecorded teardown edits.
/// An empty compiled artifact is rejected before creating an output sink.
/// `options.camera` selects the existing fixed-camera path for surfaces,
/// images and dot clouds. Camera-bearing bundles instead select the native
/// camera route automatically and replay their recorded camera/light/background
/// samples. An explicit camera is refused for those bundles, not substituted.
///
/// Sound-bearing bundles require video output. Full replay preserves audio
/// tails and pads short soundtracks to the picture duration. Use
/// [`RenderOptions::render_bundle_range`] for a precisely trimmed excerpt.
/// The loaded bundle and worker-local geometry are additional to
/// `max_resident_bytes`, which bounds planned pixels and native sink storage.
/// The production decoder bounds canonical input/snapshot storage. The receipt
/// is not a full input-closure certification or video determinism promise.
///
/// # Errors
/// Preserves bundle/schema/engine failures and normal native output failures.
/// Earlier private frames are aborted on reconstruction, raster or sink error.
pub fn render_bundle(bytes: &[u8], options: RenderOptions) -> Result<RenderReport, RenderError> {
    render_bundle_with_fs(bytes, options, Arc::new(StdFs))
}

/// [`render_bundle`] with an explicit filesystem capability.
///
/// # Errors
/// Returns the same typed failures as [`render_bundle`].
pub fn render_bundle_with_fs(
    bytes: &[u8],
    options: RenderOptions,
    fs: Arc<dyn FileSystem>,
) -> Result<RenderReport, RenderError> {
    let bundle = TimelineBundle::from_bytes(bytes).map_err(RenderError::Bundle)?;
    options.render_loaded_bundle_with_fs(bundle, None, fs)
}

impl RenderOptions {
    /// Render one nonempty, zero-based, half-open recorded frame interval.
    ///
    /// Frame reconstruction uses the original global indices and recorded
    /// cameras. Output sequence numbers restart at zero; no scene callbacks,
    /// preceding frames, or source audio files are evaluated. `max_frames`
    /// bounds the selected interval, not its unrendered prefix.
    ///
    /// An attached soundtrack is cut on its own absolute sample grid. The
    /// two picture boundaries are converted independently with the established
    /// rational clock, so adjacent excerpts never lose or duplicate a sample.
    /// Missing samples are silence. Even `0..frame_count` is an explicit cut:
    /// unlike full replay, it does not retain an audio tail beyond the selection.
    /// PCM rate, channels and stored sample bits are never changed by the cut.
    ///
    /// The scene report retains the absolute ending frame, counts intersected
    /// nonempty segments, and marks an end before the recording's end as early.
    /// The artifact reports only emitted frames. A PNG takes the selection's
    /// last recorded sample. Picture-only formats still refuse attached audio.
    /// Video still requires an explicit [`super::FfmpegCapability`].
    ///
    /// # Errors
    /// As [`render_bundle`], plus empty, reversed or out-of-bounds intervals.
    /// Range, FPS and audio admission precede output sink creation.
    pub fn render_bundle_range(
        self,
        bytes: &[u8],
        frames: Range<u32>,
    ) -> Result<RenderReport, RenderError> {
        self.render_bundle_range_with_fs(bytes, frames, Arc::new(StdFs))
    }

    /// [`Self::render_bundle_range`] with an explicit filesystem capability.
    ///
    /// # Errors
    /// Returns the same failures as [`Self::render_bundle_range`].
    pub fn render_bundle_range_with_fs(
        self,
        bytes: &[u8],
        frames: Range<u32>,
        fs: Arc<dyn FileSystem>,
    ) -> Result<RenderReport, RenderError> {
        let bundle = TimelineBundle::from_bytes(bytes).map_err(RenderError::Bundle)?;
        self.render_loaded_bundle_with_fs(bundle, Some(frames), fs)
    }

    /// Consume an already validated recording without decoding it a second time.
    ///
    /// `None` selects ordinary full replay, including soundtrack tails.
    /// `Some(range)` has [`Self::render_bundle_range`]'s exact-cut semantics.
    /// This is the composition seam for hosts that inspect the schedule before
    /// choosing their output: ownership of pictures and sound transfers once.
    /// No implicit filesystem or process capability is acquired.
    ///
    /// # Errors
    /// Returns the same failures as the corresponding byte-input entry point.
    pub fn render_loaded_bundle_with_fs(
        mut self,
        mut bundle: TimelineBundle,
        selection: Option<Range<u32>>,
        fs: Arc<dyn FileSystem>,
    ) -> Result<RenderReport, RenderError> {
        if bundle.fps() != self.config.camera.fps {
            return Err(RenderError::InvalidOptions(
                "compiled frame rate must match export configuration",
            ));
        }
        let full_replay = selection.is_none();
        let frames = admitted_frames(bundle.frame_count(), selection, self.max_frames)?;
        if bundle.has_camera_track() {
            if self.camera.is_some() {
                return Err(RenderError::InvalidOptions(
                    "an explicit camera cannot override a recorded FMTL camera track",
                ));
            }
            self.camera = Some(self.camera_config()?);
        }
        if bundle.has_soundtrack() && !self.format.is_video() {
            return Err(RenderError::InvalidOptions(
                "sound-bearing FMTL requires MP4/MOV output; picture-only export must explicitly extract its soundtrack",
            ));
        }
        let play_count = if full_replay {
            bundle.segment_count()
        } else {
            (0..bundle.segment_count())
                .filter(|&index| {
                    bundle.segment_frame_range(index).is_some_and(|segment| {
                        segment.start < segment.end
                            && segment.start < frames.end
                            && frames.start < segment.end
                    })
                })
                .count()
        };
        let mix = bundle
            .take_soundtrack()
            .map(|pcm| compiled_soundtrack(pcm, bundle.fps(), &frames, full_replay))
            .transpose()?;
        let mut clock = RationalFrameClock::new(bundle.fps())
            .map_err(|error| RenderError::Scene(fmn_anim::AnimError::Clock(error).into()))?;
        clock
            .advance_frames(i64::from(frames.end))
            .map_err(|error| RenderError::Scene(fmn_anim::AnimError::Clock(error).into()))?;
        let scene = SceneRunReport {
            ended_early: frames.end < bundle.frame_count(),
            play_count: u64::try_from(play_count)
                .map_err(|_| RenderError::InvalidOptions("compiled segment count exceeds u64"))?,
            time: clock.now(),
        };
        let shared = bundle.into_shared().map_err(RenderError::Bundle)?;
        let first = if self.format == RenderFormat::Png {
            frames.end - 1
        } else {
            frames.start
        };
        let mut sink = RenderSink::new(self, fs)?;
        for (sequence, index) in (first..frames.end).enumerate() {
            let job = shared.frame_job(index).map_err(RenderError::Bundle)?;
            let pipeline = sink.pipeline.as_mut().ok_or(RenderError::InvalidOptions(
                "compiled pipeline was already finalized",
            ))?;
            if let Err(error) = pipeline.capture_compiled(job, sequence as u64) {
                // A source reservation can observe cancellation before the
                // coordinator sees its failing worker. Join before choosing
                // the diagnostic, without publishing private output frames.
                if let Some(emitter) = &sink.emitter {
                    emitter.cancel();
                }
                return Err(match sink.pipeline.take() {
                    Some(pipeline) => admission_failure(error, pipeline.finish()),
                    None => error,
                });
            }
        }
        let finished = sink.finish_with_soundtrack(None, mix)?;
        Ok(RenderReport {
            scene,
            artifact: finished.artifact,
            emission: finished.emission,
            execution_plan: sink.plan.clone(),
            frame_pipeline: finished.frame_pipeline,
            typesetting: fmn_tex::TypesetSessionReport::default(),
        })
    }
}

fn admitted_frames(
    total: u32,
    selection: Option<Range<u32>>,
    maximum: u64,
) -> Result<Range<u32>, RenderError> {
    if total == 0 {
        return Err(RenderError::InvalidOptions(
            "compiled artifact has no frames to render",
        ));
    }
    let frames = selection.unwrap_or(0..total);
    if frames.start >= frames.end || frames.end > total {
        return Err(RenderError::InvalidOptions(
            "compiled frame range must be nonempty and lie inside the recording",
        ));
    }
    if u64::from(frames.end - frames.start) > maximum {
        return Err(RenderError::InvalidOptions(
            "compiled selection exceeds max_frames",
        ));
    }
    Ok(frames)
}

fn compiled_soundtrack(
    pcm: BundleSoundtrack,
    fps: u32,
    frames: &Range<u32>,
    full_replay: bool,
) -> Result<fmn_output::MixReport, RenderError> {
    let sample_rate = pcm.sample_rate();
    let channels = pcm.channels();
    let boundary = |frame| {
        let samples = fmn_output::frames_to_samples(i64::from(frame), fps, sample_rate)
            .map_err(RenderError::Sound)?;
        u64::try_from(samples)
            .map_err(|_| RenderError::InvalidOptions("negative compiled soundtrack boundary"))
    };
    let end = boundary(frames.end)?;
    let pcm = if full_replay {
        pcm.pad_to(end, fmn_scene::DEFAULT_MAX_BUNDLE_BYTES)
    } else {
        let start = boundary(frames.start)?;
        let count = end.checked_sub(start).ok_or(RenderError::InvalidOptions(
            "compiled soundtrack interval is reversed",
        ))?;
        pcm.sample_window(start, count, fmn_scene::DEFAULT_MAX_BUNDLE_BYTES)
    }
    .map_err(RenderError::Bundle)?;
    Ok(fmn_output::MixReport {
        audio: fmn_codec::WavAudio {
            channels,
            sample_rate,
            format: fmn_codec::SampleFormat::F32,
            samples: pcm.into_samples(),
        },
        clipped_samples: 0,
        cues_mixed: 1,
        workers_used: 0,
        kernel: fmn_output::MixKernel::Scalar,
    })
}

// A real worker failure discovered while joining carries the root cause.
// Reservation cancellation during cleanup is not an admission diagnostic.
pub(super) fn admission_failure(
    admission: RenderError,
    joined: Result<PipelineStats, RenderError>,
) -> RenderError {
    match joined {
        Err(RenderError::Pipeline(FrameStreamError::Pipeline(failure)))
            if matches!(&failure.error, PipelineError::Cancelled) =>
        {
            admission
        }
        Err(error) => error,
        Ok(_) => admission,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pcm() -> BundleSoundtrack {
        BundleSoundtrack::new(
            10,
            2,
            (0_u8..30)
                .flat_map(|n| [f32::from(n) / 32.0, -f32::from(n) / 32.0])
                .collect(),
        )
        .unwrap()
    }

    #[test]
    fn adjacent_excerpts_partition_the_absolute_sample_grid() {
        // 10 Hz / 3 fps: absolute boundaries round to 0, 3, 7, 10.
        // Rounding three independent durations would incorrectly yield 9.
        let a = compiled_soundtrack(pcm(), 3, &(0..1), false).unwrap();
        let b = compiled_soundtrack(pcm(), 3, &(1..2), false).unwrap();
        let c = compiled_soundtrack(pcm(), 3, &(2..3), false).unwrap();
        assert_eq!(
            (a.audio.samples.len(), b.audio.samples.len(), c.audio.samples.len()),
            (6, 8, 6)
        );
        let joined: Vec<_> = a.audio.samples.into_iter()
            .chain(b.audio.samples)
            .chain(c.audio.samples)
            .collect();
        assert_eq!(joined, pcm().samples()[..20]);
        assert_eq!(b.workers_used, 0);
        assert_eq!(b.audio.sample_rate, 10);
        assert_eq!(b.audio.channels, 2);
    }

    #[test]
    fn full_replay_preserves_tails_but_explicit_full_range_cuts_them() {
        let full = compiled_soundtrack(pcm(), 3, &(0..3), true).unwrap();
        let cut = compiled_soundtrack(pcm(), 3, &(0..3), false).unwrap();
        assert_eq!(full.audio.samples, pcm().samples());
        assert_eq!(cut.audio.samples, pcm().samples()[..20]);
    }

    #[test]
    fn short_and_late_audio_windows_pad_only_the_requested_interval() {
        let pcm = BundleSoundtrack::new(10, 1, vec![0.25; 4]).unwrap();
        let mixed = compiled_soundtrack(pcm, 3, &(1..2), false).unwrap();
        assert_eq!(mixed.audio.samples, [0.25, 0.0, 0.0, 0.0]);
        let pcm = BundleSoundtrack::new(10, 1, vec![0.25]).unwrap();
        let mixed = compiled_soundtrack(pcm, 3, &(1000..1001), false).unwrap();
        assert_eq!(mixed.audio.samples, [0.0; 4]);
    }

    #[test]
    fn frame_admission_has_no_prefix_charge_or_empty_range_escape() {
        assert_eq!(admitted_frames(1000, Some(997..1000), 3).unwrap(), 997..1000);
        assert!(admitted_frames(1000, None, 3).is_err());
        assert!(admitted_frames(1000, Some(997..1000), 2).is_err());
        assert!(admitted_frames(0, None, 1).is_err());
        for (start, end) in [(0, 0), (2, 1), (1, 1001), (1000, 1001)] {
            assert!(admitted_frames(1000, Some(start..end), 1001).is_err());
        }
    }
}
