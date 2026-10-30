//! Native sound-cue composition, standalone WAV export, and input receipts.

use std::collections::BTreeMap;
use std::collections::btree_map::Entry;
use std::num::NonZeroUsize;
use std::path::PathBuf;
use std::sync::Arc;

use fmn_anim::FramePacket;
use fmn_codec::SampleFormat;
use fmn_config::Config;
use fmn_output::{
    AudioDecodeLimits, AudioDecodeReport, AudioDecoder, DecodedAudio, DitherPolicy,
    InvocationReport, MixReport, MixerConfig, SoundCue, SoundMixer, WavPublicationConfig,
    WavPublicationReport, frames_to_samples, publish_wav_new,
};
use fmn_platform::clock::StdClock;
use fmn_platform::fs::{FileSystem, StdFs};
use fmn_scene::{
    CaptureReason, IntegrationError, LifecycleEvent, RuntimeConfig, SceneRunReport, SceneSink,
    SoundRequest,
};
use fmn_tex::{TexSession, TypesetSessionReport};

use super::RenderError;
use crate::SceneConstruct;

const MAX_SOUND_ASSET_BYTES: usize = 64 * 1024 * 1024;
const MAX_OMITTED_INTERVALS: usize = 1_000_000;

/// Map the observed scene frame grid to a video's output frame grid.
///
/// Skipped intervals advance scene time without entering the movie. Observe
/// lifecycle boundaries in order, with the skip status that applies after
/// each boundary. All observations and mapped cues use the same scene grid.
/// Cue-relative offsets remain separate and are applied once by the mixer.
#[derive(Clone, Debug, Default)]
pub struct SoundtrackTimeline {
    last_frame: i64,
    skipping: bool,
    omitted: Vec<(i64, i64)>,
}

impl SoundtrackTimeline {
    /// Observe a nonnegative scene frame and its following skip status.
    ///
    /// # Errors
    /// A clock rewind or an exhausted interval/allocation budget. Rejected
    /// observations do not change the timeline.
    pub fn observe(&mut self, frame: i64, skipping: bool) -> Result<(), RenderError> {
        if frame < self.last_frame {
            return Err(RenderError::InvalidOptions(
                "soundtrack timeline cannot rewind an active output generation",
            ));
        }
        if self.skipping && frame > self.last_frame {
            if let Some(last) = self.omitted.last_mut()
                && last.1 == self.last_frame
            {
                last.1 = frame;
            } else {
                if self.omitted.len() >= MAX_OMITTED_INTERVALS {
                    return Err(RenderError::InvalidOptions(
                        "soundtrack skipped-interval budget exhausted",
                    ));
                }
                self.omitted.try_reserve(1).map_err(|_| {
                    RenderError::InvalidOptions("soundtrack interval allocation failed")
                })?;
                self.omitted.push((self.last_frame, frame));
            }
        }
        self.last_frame = frame;
        self.skipping = skipping;
        Ok(())
    }

    /// Rebase one observed scene-frame position after removing skipped time.
    ///
    /// # Errors
    /// A negative position or one beyond the latest observation.
    pub fn output_frame(&self, frame: i64) -> Result<i64, RenderError> {
        if frame < 0 || frame > self.last_frame {
            return Err(RenderError::InvalidOptions(
                "soundtrack timestamp is outside the observed scene timeline",
            ));
        }
        // Ordered, disjoint intervals lie within [0, last_frame], so their
        // intersection with [0, frame] cannot exceed frame or overflow i64.
        let removed: i64 = self
            .omitted
            .iter()
            .map(|&(start, end)| (frame.min(end) - start).max(0))
            .sum();
        Ok(frame - removed)
    }
}

/// Exact source bytes and native decode identity of one insertion-ordered cue.
///
/// Shared bytes let several subdivision receipts name the same prepared input
/// without copying its buffer. Provenance may use `report.source_digest`
/// directly; it must not reopen the authored path after decoding.
#[derive(Clone, Debug)]
pub struct SoundtrackInput {
    pub path: PathBuf,
    pub bytes: Arc<[u8]>,
    pub report: AudioDecodeReport,
}

/// Cumulative admission limits for retained audio preparation, independent of
/// the final mixed duration. Repeated cues share source reads/decodes but each
/// retains its own mapped/resampled f64 samples.
#[derive(Clone, Copy, Debug)]
pub struct SoundtrackPreparationLimits {
    /// Insertion-ordered requests, checked before reading any source.
    pub max_cues: usize,
    /// Total bytes retained for distinct source paths, not repeated cues.
    pub max_source_bytes: usize,
    /// Total cached interleaved f32 samples; each decoder has its own limit too.
    pub max_decoded_samples: usize,
    /// Total mapped/resampled interleaved f64 samples, charged for every cue.
    pub max_prepared_samples: usize,
}

impl SoundtrackPreparationLimits {
    /// Preserve the original preparation API's per-source and mixer limits.
    pub const UNBOUNDED: Self = Self {
        max_cues: usize::MAX,
        max_source_bytes: usize::MAX,
        max_decoded_samples: usize::MAX,
        max_prepared_samples: usize::MAX,
    };
}

impl Default for SoundtrackPreparationLimits {
    fn default() -> Self {
        Self {
            max_cues: 4096,
            max_source_bytes: 256 * 1024 * 1024,
            max_decoded_samples: 16 * 1024 * 1024,
            max_prepared_samples: 32 * 1024 * 1024,
        }
    }
}

fn charge(
    current: &mut usize,
    additional: usize,
    limit: usize,
    message: &'static str,
) -> Result<(), RenderError> {
    let next = current
        .checked_add(additional)
        .filter(|&n| n <= limit)
        .ok_or(RenderError::InvalidOptions(message))?;
    *current = next;
    Ok(())
}

/// One prepared, insertion-ordered native mix shared by complete renders and
/// absolute sample windows. Decoding, channel mapping and resampling happen
/// once; each window preserves source phase and background ducking.
#[derive(Debug)]
pub struct PreparedSceneSoundtrack {
    mixer: SoundMixer,
    inputs: Vec<SoundtrackInput>,
    invocations: Vec<InvocationReport>,
}

impl PreparedSceneSoundtrack {
    /// Read and decode the exact sound requests through caller capabilities.
    ///
    /// Each distinct authored path is read once, bounded to 64 MiB, and
    /// decoded once. Repeated cues share that immutable source snapshot even
    /// if the filesystem changes during preparation. The injected decoder owns
    /// native PCM decoding and any governed ffmpeg fallback. `Some(timeline)` rebases
    /// cue call sites after skipped time; `None` retains absolute scene time
    /// for phase-preserving subdivision windows. Relative offsets and gains
    /// retain the native mixer's semantics in either mode.
    ///
    /// # Errors
    /// Invalid mix/placement budgets, timestamps outside the observed grid,
    /// bounded asset reads, decode failures, or invalid PCM/gain parameters.
    pub fn new(
        fs: &dyn FileSystem,
        requests: &[SoundRequest],
        decoder: &mut AudioDecoder,
        config: MixerConfig,
        timeline: Option<&SoundtrackTimeline>,
    ) -> Result<Self, RenderError> {
        Self::new_with_limits(
            fs,
            requests,
            decoder,
            config,
            timeline,
            SoundtrackPreparationLimits::UNBOUNDED,
        )
    }

    /// Prepare through the same native mixer with cumulative source, decoded
    /// and resampled storage bounds. Cue-count refusal precedes filesystem work;
    /// raw reads use the remaining budget, and mapped storage is charged before
    /// each cue is cloned/resampled. The decoder separately caps each decode.
    ///
    /// # Errors
    /// The original typed asset/decode/mix failures or a named cumulative limit.
    pub fn new_with_limits(
        fs: &dyn FileSystem,
        requests: &[SoundRequest],
        decoder: &mut AudioDecoder,
        config: MixerConfig,
        timeline: Option<&SoundtrackTimeline>,
        limits: SoundtrackPreparationLimits,
    ) -> Result<Self, RenderError> {
        if requests.len() > limits.max_cues {
            return Err(RenderError::InvalidOptions(
                "soundtrack cue-count budget exceeded",
            ));
        }
        let mut source_bytes = 0;
        let mut decoded_samples = 0;
        let mut prepared_samples = 0;
        let mut mixer = SoundMixer::new(config).map_err(RenderError::Sound)?;
        let mut inputs = Vec::new();
        inputs
            .try_reserve_exact(requests.len())
            .map_err(|_| RenderError::InvalidOptions("soundtrack input allocation failed"))?;
        let mut sources: BTreeMap<PathBuf, (Arc<[u8]>, DecodedAudio)> = BTreeMap::new();
        let mut invocations = Vec::new();
        for request in requests {
            let frame = match timeline {
                Some(timeline) => timeline.output_frame(request.time.frames())?,
                None => request.time.frames(),
            };
            let (bytes, decoded) = match sources.entry(request.sound_file.clone()) {
                Entry::Occupied(entry) => entry.into_mut(),
                Entry::Vacant(entry) => {
                    let bytes = fs
                        .read_bounded(
                            &request.sound_file,
                            MAX_SOUND_ASSET_BYTES.min(limits.max_source_bytes - source_bytes),
                        )
                        .map_err(|error| RenderError::SoundAsset {
                            path: request.sound_file.clone(),
                            error,
                        })?;
                    charge(
                        &mut source_bytes,
                        bytes.len(),
                        limits.max_source_bytes,
                        "soundtrack source-byte budget exceeded",
                    )?;
                    let decoded =
                        decoder
                            .decode(&bytes)
                            .map_err(|error| RenderError::SoundDecode {
                                path: request.sound_file.clone(),
                                error,
                            })?;
                    charge(
                        &mut decoded_samples,
                        decoded.audio.samples.len(),
                        limits.max_decoded_samples,
                        "soundtrack decoded-sample budget exceeded",
                    )?;
                    if let Some(invocation) = decoded.report.invocation.clone() {
                        invocations.try_reserve(1).map_err(|_| {
                            RenderError::InvalidOptions("soundtrack invocation allocation failed")
                        })?;
                        invocations.push(invocation);
                    }
                    entry.insert((Arc::from(bytes), decoded))
                }
            };
            // Budget sizing only: the native mixer's resampler owns the actual
            // channel mapping and samples. Its output uses ceil(n * out / in).
            let input_frames = decoded.audio.samples.len() / usize::from(decoded.audio.channels);
            let output_frames = ((input_frames as u128) * u128::from(config.sample_rate))
                .div_ceil(u128::from(decoded.audio.sample_rate));
            let count = output_frames
                .checked_mul(u128::from(config.channels))
                .and_then(|n| usize::try_from(n).ok())
                .ok_or(RenderError::InvalidOptions(
                    "soundtrack prepared-sample count overflow",
                ))?;
            charge(
                &mut prepared_samples,
                count,
                limits.max_prepared_samples,
                "soundtrack prepared-sample budget exceeded",
            )?;
            mixer
                .add(SoundCue {
                    audio: decoded.audio.clone(),
                    frame,
                    fps: request.time.fps(),
                    time_offset: request.time_offset,
                    gain: request.gain,
                    gain_to_background: request.gain_to_background,
                })
                .map_err(RenderError::Sound)?;
            inputs.push(SoundtrackInput {
                path: request.sound_file.clone(),
                bytes: Arc::clone(bytes),
                report: decoded.report.clone(),
            });
        }
        Ok(Self {
            mixer,
            inputs,
            invocations,
        })
    }

    /// Set the complete mix's minimum duration, counted in output sample
    /// frames. Explicit windows retain their own exact length.
    #[must_use]
    pub fn with_timeline_frames(mut self, sample_frames: u64) -> Self {
        self.mixer = self.mixer.with_timeline_frames(sample_frames);
        self
    }

    /// Mix the complete prepared timeline, including cue tails.
    ///
    /// # Errors
    /// The native mixer's worker, output-budget and sample-accumulation errors.
    pub fn mix(&self, threads: usize) -> Result<MixReport, RenderError> {
        self.mixer.mix(threads).map_err(RenderError::Sound)
    }

    /// Mix exactly one half-open interval of the absolute output-sample grid.
    /// No prefix or future tail is allocated. Convert the start and end scene
    /// positions independently with `frames_to_samples` to avoid rounding
    /// gaps between adjacent clips on non-dividing frame rates.
    ///
    /// # Errors
    /// The native mixer's range, worker, output-budget and accumulation errors.
    pub fn mix_window(
        &self,
        start_sample_frame: u64,
        sample_frames: u64,
        threads: usize,
    ) -> Result<MixReport, RenderError> {
        self.mixer
            .mix_window(start_sample_frame, sample_frames, threads)
            .map_err(RenderError::Sound)
    }

    /// Inputs in authored cue order, retaining the bytes actually decoded.
    #[must_use]
    pub fn inputs(&self) -> &[SoundtrackInput] {
        &self.inputs
    }

    /// Actual media-decode invocations in first-source-use order; repeated
    /// uses of one cached source do not repeat its invocation. These may overlap an
    /// already-running video encoder and must be reported separately from
    /// that boundary's encode/mux execution order. Native PCM WAV adds none.
    #[must_use]
    pub fn invocations(&self) -> Vec<InvocationReport> {
        self.invocations.clone()
    }
}

/// Native WAV export of an ordinary scene's sound requests.
///
/// The scene still executes its normal animations and updaters. No pixel
/// buffers, render teams, or video encoder are created. Input is native PCM
/// WAV; compressed media returns the decoder's named capability refusal.
/// Output is canonical signed 16-bit PCM without dither, at the explicit
/// mixer's sample rate and mono/stereo layout. AAC/video quality settings
/// are irrelevant to this native artifact and are not consulted.
#[derive(Debug, Clone)]
pub struct SoundtrackRenderOptions {
    /// New file destination. An existing file is never replaced.
    pub output: PathBuf,
    /// Existing scene clock, seed and native typesetting configuration.
    /// Camera resolution, raster quality and render engine are not used.
    pub config: Config,
    /// Output sample rate, mono/stereo layout and per-cue/timeline frame bound.
    /// The output-byte ceiling below can impose a smaller sample-frame bound.
    pub mixer: MixerConfig,
    /// Per-source native WAV decoding bounds. No transcoder is admitted.
    pub decode: AudioDecodeLimits,
    /// Cumulative source, decoded and per-cue prepared-storage admission.
    pub preparation: SoundtrackPreparationLimits,
    /// Nonzero ceiling for mixer workers. Reported workers are those used.
    pub threads: NonZeroUsize,
    /// Maximum absolute scene time in frame-clock ticks, and maximum number
    /// of capture requests. Skipped time also counts against the clock limit.
    pub max_scene_frames: u64,
    /// Complete WAV artifact ceiling, including its 44-byte RIFF header.
    /// Mixer allocation is bounded to the representable S16 payload first.
    /// Source decoding retains its existing 64 MiB per-file input bound.
    pub max_output_bytes: u64,
    /// Attach the configured persistent typeset cache if construction uses it.
    pub typeset_cache: bool,
    /// Ceiling for the scene's declared native typesetting preflight.
    pub typeset_preflight_workers: NonZeroUsize,
}

impl SoundtrackRenderOptions {
    /// Bundled scene defaults, 48 kHz stereo, and one native mixer worker.
    ///
    /// # Errors
    /// Returns the existing configuration error if bundled defaults fail.
    pub fn new(output: impl Into<PathBuf>) -> Result<Self, fmn_config::ConfigError> {
        Ok(Self {
            output: output.into(),
            config: Config::resolve(&[], None)?.config,
            mixer: MixerConfig::default(),
            decode: AudioDecodeLimits::default(),
            preparation: SoundtrackPreparationLimits::default(),
            threads: NonZeroUsize::MIN,
            max_scene_frames: 1_000_000,
            max_output_bytes: 64 * 1024 * 1024,
            typeset_cache: true,
            typeset_preflight_workers: crate::typesetting::DEFAULT_PREFLIGHT_WORKERS,
        })
    }

    fn admitted_mixer(&self) -> Result<MixerConfig, RenderError> {
        if self.output.as_os_str().is_empty() || self.output.file_name().is_none() {
            return Err(RenderError::InvalidOptions(
                "soundtrack output must name a file",
            ));
        }
        if self.max_scene_frames == 0 {
            return Err(RenderError::InvalidOptions(
                "soundtrack scene-frame limit must be nonzero",
            ));
        }
        if self.preparation.max_cues == 0
            || self.preparation.max_source_bytes == 0
            || self.preparation.max_decoded_samples == 0
            || self.preparation.max_prepared_samples == 0
        {
            return Err(RenderError::InvalidOptions(
                "soundtrack preparation budgets must be nonzero",
            ));
        }
        // Validate the authored channel/rate/timeline policy before using any
        // fields in byte-budget arithmetic, and before executing scene code.
        SoundMixer::new(self.mixer)
            .and_then(|mixer| mixer.mix(self.threads.get()))
            .map_err(RenderError::Sound)?;
        if self
            .mixer
            .sample_rate
            .checked_mul(u32::from(self.mixer.channels) * 2)
            .is_none()
        {
            return Err(RenderError::InvalidOptions(
                "soundtrack sample rate exceeds WAV byte-rate representation",
            ));
        }
        let payload = self
            .max_output_bytes
            .checked_sub(44)
            .ok_or(RenderError::InvalidOptions(
                "soundtrack byte budget is smaller than a WAV header",
            ))?;
        // Native WAV uses RIFF's 32-bit size. Its data length plus the other
        // 36 bytes must fit that representation even on a 64-bit host.
        let payload = payload.min(u64::from(u32::MAX) - 36);
        let frames = payload / (u64::from(self.mixer.channels) * 2);
        if frames == 0 {
            return Err(RenderError::InvalidOptions(
                "soundtrack byte budget must hold at least one PCM sample frame",
            ));
        }
        Ok(MixerConfig {
            max_output_frames: self.mixer.max_output_frames.min(frames),
            ..self.mixer
        })
    }

    fn decoder(&self) -> Result<AudioDecoder, RenderError> {
        let mut limits = self.decode.clone();
        limits.max_input_bytes = limits
            .max_input_bytes
            .min(self.preparation.max_source_bytes as u64);
        limits.max_samples = limits
            .max_samples
            .min(self.preparation.max_decoded_samples as u64);
        AudioDecoder::new(limits, None).map_err(|error| RenderError::SoundDecode {
            path: PathBuf::new(),
            error,
        })
    }
}

/// Completed native WAV and the scene that authored it.
///
/// Audio reports sample frames and actual mixer workers; it has no fictitious
/// pixel pipeline or video-frame count. This is an artifact receipt, not a
/// complete certified input-closure manifest for caller-authored scene code.
#[derive(Debug, Clone)]
pub struct SoundtrackRenderReport {
    /// Exact rational scene time and play count.
    pub scene: SceneRunReport,
    /// Published WAV path, digest, sample frames, cue count and clipping count.
    pub artifact: WavPublicationReport,
    /// Native output sample rate.
    pub sample_rate: u32,
    /// Native output channels (one or two).
    pub channels: u16,
    /// Actual native mixer workers, zero for an empty mix.
    pub workers_used: usize,
    /// Insertion-ordered exact source bytes and decoder identities. Repeated
    /// cues share the retained source bytes; no input is reopened for receipts.
    pub inputs: Vec<SoundtrackInput>,
    /// Actual native typesetting/cache work during scene construction.
    pub typesetting: TypesetSessionReport,
}

/// Execute a native scene and publish its complete soundtrack as PCM WAV.
///
/// The normal scene clock, updater ordering, early completion, cue offsets,
/// gains and background ducking remain authoritative. Skipped intervals are
/// removed using the same timeline mapping as native video; cue tails extend
/// the output past the scene when necessary. A scene with no cues emits
/// digital silence spanning its non-skipped duration.
///
/// No renderer or external process is used. PCM source files are decoded by
/// the native codec, then the native mixer and create-only WAV publication
/// complete before the receipt is returned. Scene, source and budget failures
/// leave the destination unpublished; existing files survive all failures.
///
/// # Errors
/// Invalid configuration, scene execution, native source decode/mix, budget,
/// or create-only publication errors, preserving their existing typed source.
pub fn render_soundtrack<P: SceneConstruct + ?Sized>(
    program: &mut P,
    options: SoundtrackRenderOptions,
) -> Result<SoundtrackRenderReport, RenderError> {
    render_soundtrack_with_fs(program, options, Arc::new(StdFs))
}

/// [`render_soundtrack`] with an explicit filesystem capability.
///
/// # Errors
/// Returns the same typed errors as [`render_soundtrack`].
pub fn render_soundtrack_with_fs<P: SceneConstruct + ?Sized>(
    program: &mut P,
    options: SoundtrackRenderOptions,
    fs: Arc<dyn FileSystem>,
) -> Result<SoundtrackRenderReport, RenderError> {
    let mixer = options.admitted_mixer()?;
    let mut decoder = options.decoder()?;
    let runtime = RuntimeConfig::from_config(&options.config);
    let typesetting = if options.typeset_cache {
        TexSession::with_cache(&options.config, Arc::clone(&fs), Arc::new(StdClock::new()))
    } else {
        TexSession::memory(options.config.tex.template.clone())
    };
    let mut sink = SoundtrackCapture {
        timeline: SoundtrackTimeline::default(),
        max_frames: options.max_scene_frames,
        captures: 0,
        failure: None,
    };
    let completed = crate::run_scene_with_typography(
        program,
        runtime,
        options.config.determinism.seed,
        &mut sink,
        &typesetting,
        &options.config.text,
        options.typeset_preflight_workers,
    );
    let completed = completed.map_err(|error| {
        // Scene::run preserves authored failures over later cleanup failures.
        // Recover our typed cause only for the integration error we emitted.
        if matches!(&error, crate::Error::Scene(fmn_scene::SceneError::Integration(integration))
            if integration.point() == "native-soundtrack")
            && let Some(failure) = sink.failure.take()
        {
            return failure;
        }
        RenderError::Scene(error)
    })?;
    // Catching a capture error in authored code cannot publish a partial mix.
    if let Some(error) = sink.failure.take() {
        return Err(error);
    }
    let scene = completed.scene();
    sink.observe_time(scene.time().frames(), scene.is_skipping())?;
    let requests = scene.sound_requests();
    let frames = sink.timeline.output_frame(scene.time().frames())?;
    let samples = frames_to_samples(frames, scene.time().fps(), mixer.sample_rate)
        .map_err(RenderError::Sound)?;
    let samples = u64::try_from(samples)
        .map_err(|_| RenderError::InvalidOptions("soundtrack timeline is negative"))?;
    if samples > mixer.max_output_frames {
        return Err(RenderError::Sound(fmn_output::SoundError::OutputTooLong {
            frames: samples,
            max_frames: mixer.max_output_frames,
        }));
    }
    let prepared = PreparedSceneSoundtrack::new_with_limits(
        fs.as_ref(),
        requests,
        &mut decoder,
        mixer,
        Some(&sink.timeline),
        options.preparation,
    )?
    .with_timeline_frames(samples);
    let mix = prepared.mix(options.threads.get())?;
    let artifact = publish_wav_new(
        fs.as_ref(),
        &WavPublicationConfig {
            destination: options.output,
            format: SampleFormat::S16,
            dither: DitherPolicy::None,
            max_artifact_bytes: options.max_output_bytes,
            profile: None,
        },
        &mix,
    )
    .map_err(RenderError::Sink)?;
    Ok(SoundtrackRenderReport {
        scene: *completed.report(),
        artifact,
        sample_rate: mix.audio.sample_rate,
        channels: mix.audio.channels,
        workers_used: mix.workers_used,
        inputs: prepared.inputs().to_vec(),
        typesetting: completed.typesetting_report().clone(),
    })
}

/// Observe the actual runtime with no alternate animation or audio clock.
struct SoundtrackCapture {
    timeline: SoundtrackTimeline,
    max_frames: u64,
    captures: u64,
    failure: Option<RenderError>,
}

impl SoundtrackCapture {
    fn observe_time(&mut self, frames: i64, skipping: bool) -> Result<(), RenderError> {
        if u64::try_from(frames)
            .ok()
            .is_none_or(|frames| frames > self.max_frames)
        {
            return Err(RenderError::InvalidOptions(
                "scene exceeded max_scene_frames",
            ));
        }
        self.timeline.observe(frames, skipping)
    }

    fn observe_captures(&mut self, count: u64) -> Result<(), RenderError> {
        let frames = self
            .captures
            .checked_add(count)
            .filter(|frames| *frames <= self.max_frames)
            .ok_or(RenderError::InvalidOptions(
                "scene exceeded max_scene_frames",
            ))?;
        self.captures = frames;
        Ok(())
    }

    fn retain_error(&mut self, result: Result<(), RenderError>) -> Result<(), IntegrationError> {
        result.map_err(|error| {
            let message = error.to_string();
            if self.failure.is_none() {
                self.failure = Some(error);
            }
            IntegrationError::new("native-soundtrack", message)
        })
    }
}

impl SceneSink for SoundtrackCapture {
    fn capture_state_required(&self) -> bool {
        false
    }

    fn capture_metadata(
        &mut self,
        _: CaptureReason,
        _: fmn_anim::FrameMetadata,
    ) -> Result<(), IntegrationError> {
        let result = self.observe_captures(1);
        self.retain_error(result)
    }

    fn event(&mut self, event: LifecycleEvent) -> Result<(), IntegrationError> {
        let result = self.observe_time(event.time.frames(), event.skipping);
        self.retain_error(result)
    }

    fn capture(&mut self, _: CaptureReason, _: FramePacket) -> Result<(), IntegrationError> {
        let result = self.observe_captures(1);
        self.retain_error(result)
    }

    fn accepts_pure_segments(&self) -> bool {
        self.failure.is_none()
    }

    fn capture_pure_segment(
        &mut self,
        segment: &fmn_anim::pure_frame::PureSegment,
    ) -> Result<(), IntegrationError> {
        let result = u64::try_from(segment.frame_count())
            .map_err(|_| RenderError::InvalidOptions("pure segment has a negative frame count"))
            .and_then(|count| self.observe_captures(count));
        self.retain_error(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fmn_anim::RationalTime;
    use fmn_codec::{SampleFormat, WavAudio, encode_wav};
    use fmn_output::{AudioDecodeLimits, AudioInputFormat, frames_to_samples};
    use fmn_platform::fs::VirtualFs;

    fn config() -> MixerConfig {
        MixerConfig {
            sample_rate: 8,
            channels: 1,
            max_output_frames: 1_000,
        }
    }

    fn request(path: &str, frame: i64, fps: u32) -> SoundRequest {
        SoundRequest {
            sound_file: path.into(),
            time: RationalTime::zero(fps) + frame,
            time_offset: 0.0,
            gain: None,
            gain_to_background: None,
        }
    }

    fn native_decoder() -> AudioDecoder {
        AudioDecoder::new(AudioDecodeLimits::default(), None).unwrap()
    }

    #[test]
    fn skipped_intervals_rebase_call_sites_without_restarting_time() {
        let mut timeline = SoundtrackTimeline::default();
        for (frame, skipping) in [
            (0, true),
            (12, false),
            (20, true),
            (24, true),
            (30, false),
            (36, false),
        ] {
            timeline.observe(frame, skipping).unwrap();
        }
        assert_eq!(timeline.omitted, [(0, 12), (20, 30)]);
        for (scene, output) in [
            (0, 0),
            (11, 0),
            (12, 0),
            (18, 6),
            (20, 8),
            (25, 8),
            (30, 8),
            (36, 14),
        ] {
            assert_eq!(timeline.output_frame(scene).unwrap(), output);
        }
        assert!(timeline.output_frame(-1).is_err());
        assert!(timeline.output_frame(37).is_err());
        assert!(timeline.observe(35, false).is_err());
        assert_eq!(timeline.output_frame(36).unwrap(), 14);
    }

    #[test]
    fn native_sources_are_snapshotted_and_rebased_before_offset_application() {
        let fs = VirtualFs::new();
        let bytes = encode_wav(1, 8, SampleFormat::F32, &[0.125, 0.25, 0.375, 0.5]);
        fs.insert("/tone.wav", bytes.clone());
        let mut timeline = SoundtrackTimeline::default();
        timeline.observe(0, true).unwrap();
        timeline.observe(4, false).unwrap();
        timeline.observe(10, false).unwrap();
        let mut cue = request("/tone.wav", 6, 8);
        cue.time_offset = -0.125;
        let prepared = PreparedSceneSoundtrack::new(
            &fs,
            &[cue],
            &mut native_decoder(),
            config(),
            Some(&timeline),
        )
        .unwrap()
        .with_timeline_frames(6);
        fs.insert("/tone.wav", b"changed after decode".to_vec());
        let mix = prepared.mix(1).unwrap();
        assert_eq!(mix.audio.samples, [0.0, 0.125, 0.25, 0.375, 0.5, 0.0]);
        assert_eq!(prepared.inputs()[0].bytes.as_ref(), bytes.as_slice());
        assert_eq!(prepared.inputs()[0].path, PathBuf::from("/tone.wav"));
        assert_eq!(prepared.inputs()[0].report.format, AudioInputFormat::Wav);
        assert!(prepared.invocations().is_empty());
    }

    #[test]
    fn repeated_cues_share_one_snapshot_without_losing_their_placements_or_gains() {
        let fs = VirtualFs::new();
        fs.insert(
            "/repeat.wav",
            encode_wav(1, 8, SampleFormat::F32, &[0.25, 0.5]),
        );
        let first = request("/repeat.wav", 0, 8);
        let mut second = request("/repeat.wav", 3, 8);
        second.gain = Some(-6.020_599_913_279_624);
        let prepared = PreparedSceneSoundtrack::new(
            &fs,
            &[first, second],
            &mut native_decoder(),
            config(),
            None,
        )
        .unwrap();
        assert_eq!(prepared.inputs().len(), 2);
        assert!(Arc::ptr_eq(
            &prepared.inputs()[0].bytes,
            &prepared.inputs()[1].bytes,
        ));
        assert_eq!(
            prepared.inputs()[0].report.source_digest,
            prepared.inputs()[1].report.source_digest,
        );
        let mixed = prepared.mix(2).unwrap();
        assert_eq!(mixed.cues_mixed, 2);
        assert_eq!(mixed.audio.samples, [0.25, 0.5, 0.0, 0.125, 0.25]);
        assert!(prepared.invocations().is_empty());
    }

    #[test]
    fn rational_windows_preserve_resampling_ducking_and_whole_mix_samples() {
        let fs = VirtualFs::new();
        fs.insert(
            "/background.wav",
            encode_wav(1, 8, SampleFormat::F32, &[0.25; 16]),
        );
        fs.insert(
            "/foreground.wav",
            encode_wav(1, 11, SampleFormat::F32, &[0.125; 11]),
        );
        let background = request("/background.wav", 0, 29);
        let mut foreground = request("/foreground.wav", 4, 29);
        foreground.time_offset = -0.125;
        foreground.gain = Some(-3.0);
        foreground.gain_to_background = Some(-6.0);
        let prepared = PreparedSceneSoundtrack::new(
            &fs,
            &[background, foreground],
            &mut native_decoder(),
            MixerConfig {
                sample_rate: 48,
                ..config()
            },
            None,
        )
        .unwrap()
        .with_timeline_frames(16);
        let whole = prepared.mix(1).unwrap();
        let mut direct = SoundMixer::new(MixerConfig {
            sample_rate: 48,
            ..config()
        })
        .unwrap();
        direct
            .add(SoundCue::new(
                WavAudio {
                    channels: 1,
                    sample_rate: 8,
                    format: SampleFormat::F32,
                    samples: vec![0.25; 16],
                },
                0,
                29,
            ))
            .unwrap();
        direct
            .add(SoundCue {
                audio: WavAudio {
                    channels: 1,
                    sample_rate: 11,
                    format: SampleFormat::F32,
                    samples: vec![0.125; 11],
                },
                frame: 4,
                fps: 29,
                time_offset: -0.125,
                gain: Some(-3.0),
                gain_to_background: Some(-6.0),
            })
            .unwrap();
        assert_eq!(whole.audio.samples, direct.mix(1).unwrap().audio.samples);
        let boundaries =
            [0, 4, 8, 12, 16].map(|frame| frames_to_samples(frame, 29, 48).unwrap() as u64);
        let mut concatenated = Vec::new();
        for range in boundaries.windows(2) {
            let clip = prepared
                .mix_window(range[0], range[1] - range[0], 4)
                .unwrap();
            assert_eq!(clip.audio.samples.len() as u64, range[1] - range[0]);
            concatenated.extend(clip.audio.samples);
        }
        assert_eq!(concatenated, whole.audio.samples[..boundaries[4] as usize]);
        assert_eq!(prepared.inputs().len(), 2);
    }

    #[test]
    fn negative_offsets_clip_source_phase_and_windows_never_extend_to_a_tail() {
        let fs = VirtualFs::new();
        fs.insert(
            "/tone.wav",
            encode_wav(1, 8, SampleFormat::F32, &[0.125, 0.25, 0.375, 0.5]),
        );
        let mut cue = request("/tone.wav", 0, 8);
        cue.time_offset = -0.125;
        let prepared =
            PreparedSceneSoundtrack::new(&fs, &[cue], &mut native_decoder(), config(), None)
                .unwrap()
                .with_timeline_frames(20);
        assert_eq!(
            prepared.mix_window(0, 2, 1).unwrap().audio.samples,
            [0.25, 0.375]
        );
        assert!(
            prepared
                .mix_window(3, 0, 1)
                .unwrap()
                .audio
                .samples
                .is_empty()
        );
        assert_eq!(
            prepared.mix_window(3, 2, 1).unwrap().audio.samples,
            [0.0, 0.0]
        );
    }

    #[test]
    fn resource_and_asset_failures_keep_their_native_error_types() {
        let fs = VirtualFs::new();
        assert!(matches!(
            PreparedSceneSoundtrack::new(&fs, &[request("/missing.wav", 0, 8)], &mut native_decoder(), config(), None),
            Err(RenderError::SoundAsset { path, .. }) if path.as_path() == std::path::Path::new("/missing.wav")
        ));
        fs.insert("/invalid.wav", b"invalid media".to_vec());
        assert!(matches!(
            PreparedSceneSoundtrack::new(&fs, &[request("/invalid.wav", 0, 8)], &mut native_decoder(), config(), None),
            Err(RenderError::SoundDecode { path, .. }) if path.as_path() == std::path::Path::new("/invalid.wav")
        ));
        let prepared =
            PreparedSceneSoundtrack::new(&fs, &[], &mut native_decoder(), config(), None).unwrap();
        assert_eq!(
            prepared.mix_window(3, 2, 1).unwrap().audio.samples,
            [0.0, 0.0]
        );
        assert!(prepared.mix_window(u64::MAX, 1, 1).is_err());
        assert!(prepared.mix_window(999, 2, 1).is_err());
        assert!(prepared.mix(0).is_err());
        assert!(prepared.with_timeline_frames(1_001).mix(1).is_err());
    }
}
