//! Explicit native audio export: one real scene run, one shared mix, one bundle.
//!
//! Asset access and optional transcoding are explicit caller capabilities.
//! Native WAV needs no process; standard-mode compressed input may use the
//! governed ffmpeg boundary. Only mixed PCM enters the portable artifact.
use super::{BundleExportError, BundleExportOptions, BundleExportReport, SceneBundleExport};
use crate::rendering::{
    FfmpegCapability, PreparedSceneSoundtrack, SoundtrackInput, SoundtrackPreparationLimits,
    SoundtrackReport,
};
use crate::{SceneConstruct, rendering::RenderError};
use fmn_config::{Config, config::DeterminismMode};
use fmn_output::{
    AudioDecodeError, AudioDecodeLimits, AudioDecoder, Boundary, FfmpegTool, InvocationReport,
    JobLimits, MixerConfig, SoundMixer, frames_to_samples,
};
use fmn_platform::fs::FileSystem;
use fmn_render::CameraConfig;
use fmn_scene::CameraRig;
use fmn_scene::timeline_bundle::BundleSoundtrack;
use fmn_scene::{Scene, recording::RecordedSceneBundle};
use std::{path::Path, sync::Arc};

/// Independent audio quality/storage controls. Picture FPS and scene seed stay
/// in BundleExportOptions. Budgets do not bound arbitrary authored scene code.
#[derive(Clone, Debug)]
pub struct BundleSoundtrackOptions {
    /// Native sample clock, output channels and final mix duration ceiling.
    pub mixer: MixerConfig,
    /// Per-source input and decoded-sample bounds, including external expansion.
    pub decode: AudioDecodeLimits,
    /// Cumulative source, decoded and per-cue prepared-storage admission.
    pub preparation: SoundtrackPreparationLimits,
    /// Native mixer workers, validated before authored scene execution.
    pub workers: usize,
    /// Explicit authority to decode recognized compressed sources in standard
    /// mode. Native WAV, malformed native PCM and unknown bytes never consult
    /// this capability. The executable request and determinism mode come from
    /// [`BundleExportOptions::config`]; no ambient fallback is installed.
    pub ffmpeg: Option<FfmpegCapability>,
    /// Bounds for each governed external decode. Native decode uses `decode`.
    /// Invalid limits are refused before executing the scene or the camera factory.
    pub ffmpeg_limits: JobLimits,
}
impl Default for BundleSoundtrackOptions {
    fn default() -> Self {
        Self {
            mixer: MixerConfig::default(),
            decode: AudioDecodeLimits::default(),
            preparation: SoundtrackPreparationLimits::default(),
            workers: 1,
            ffmpeg: None,
            ffmpeg_limits: JobLimits::default(),
        }
    }
}
impl BundleSoundtrackOptions {
    fn validate(&self, config: &Config) -> Result<(), BundleExportError> {
        // Empty native work validates thread bounds without allocating PCM.
        SoundMixer::new(self.mixer)
            .and_then(|mix| mix.mix(self.workers))
            .map_err(|e| sound(RenderError::Sound(e)))?;
        if self.preparation.max_cues == 0
            || self.preparation.max_source_bytes == 0
            || self.preparation.max_decoded_samples == 0
            || self.preparation.max_prepared_samples == 0
        {
            return Err(BundleExportError::InvalidOptions(
                "soundtrack preparation budgets must be nonzero",
            ));
        }
        self.decoder(config)?;
        Ok(())
    }
    fn decoder(&self, config: &Config) -> Result<AudioDecoder, BundleExportError> {
        let mut limits = self.decode.clone();
        limits.max_input_bytes = limits
            .max_input_bytes
            .min(self.preparation.max_source_bytes as u64);
        limits.max_samples = limits
            .max_samples
            .min(self.preparation.max_decoded_samples as u64);
        let decode_error = |error| {
            sound(RenderError::SoundDecode {
                path: Default::default(),
                error,
            })
        };
        // Validate both independent policies without resolving a tool. The
        // decoder still enforces its sample-derived expansion ceiling inside
        // Boundary::decode_audio_bytes after the capability is resolved.
        let job_limits = fmn_output::admit_render_ffmpeg_limits(
            &self.ffmpeg_limits,
            self.ffmpeg_limits.max_artifact_bytes,
        )
        .map_err(|error| decode_error(AudioDecodeError::InvalidJobLimits(error)))?;
        let native = AudioDecoder::new(limits.clone(), None).map_err(decode_error)?;
        let capability = self.ffmpeg.clone();
        let certified = config.determinism.mode == DeterminismMode::Certified;
        if capability.is_none() && !certified {
            return Ok(native);
        }
        let executable = config.file_writer.ffmpeg_bin.clone();
        if capability.is_some() && (executable.is_empty() || executable.contains('\0')) {
            return Err(BundleExportError::InvalidOptions(
                "soundtrack ffmpeg executable must not be empty or contain NUL",
            ));
        }
        AudioDecoder::with_boundary_resolver(limits, move |format| {
            if certified {
                return Err(AudioDecodeError::Capability(format!(
                    "{} decoding through ffmpeg is outside certified bundle preparation; supply native PCM WAV",
                    format.demuxer(),
                )));
            }
            let capability = capability
                .as_ref()
                .ok_or(AudioDecodeError::TranscoderRequired { format })?;
            let located = capability
                .locator
                .locate_ffmpeg(Path::new(&executable))
                .map_err(|error| AudioDecodeError::Capability(error.to_string()))?;
            let tool = FfmpegTool::resolve(
                located,
                capability.runner.as_ref(),
                &capability.workdir_root,
            )?;
            Boundary::new(
                tool,
                Arc::clone(&capability.runner),
                job_limits.clone(),
                capability.workdir_root.clone(),
            )
            .map_err(AudioDecodeError::Boundary)
        })
        .map_err(decode_error)
    }
}

/// Audio actually embedded in a completed native bundle, with its input receipts.
///
/// These receipts describe the exact sources and any governed decoder work;
/// they do not certify arbitrary scene inputs or external compressed decoding.
/// Playback needs only the bundle's PCM, never these source bytes or paths.
#[derive(Clone, Debug)]
pub struct BundleSoundtrackReport {
    /// Native mix shape, cue count and clipping count.
    pub mix: SoundtrackReport,
    /// Actual native mixer workers, independent of picture capture policy.
    pub workers_used: usize,
    /// Exact source snapshots and decoder identities in authored cue order.
    /// Repeated cues share their source allocation; receipts never reopen paths.
    pub inputs: Vec<SoundtrackInput>,
    /// Governed decode invocations in first-source-use order. Native WAV adds
    /// none, and repeated paths do not add another external invocation.
    pub audio_decodes: Vec<InvocationReport>,
}

fn sound(error: RenderError) -> BundleExportError {
    BundleExportError::Soundtrack(error)
}

/// Record actual native captures and mix their authored sound cues.
///
/// No cue means byte-identical legacy output and no asset reads. Cues retain
/// rational call sites, negative offsets, gains, background ducking, channel
/// mapping and resampling through PreparedSceneSoundtrack. Missing/unsupported
/// sources or exhausted budgets produce no partial artifact.
///
/// # Errors
/// Native scene/capture errors plus typed audio, allocation and output refusals.
pub fn export_bundle_with_soundtrack_bytes<P: SceneConstruct + ?Sized>(
    program: &mut P,
    options: BundleExportOptions,
    audio: &BundleSoundtrackOptions,
    assets: &dyn FileSystem,
) -> Result<SceneBundleExport, BundleExportError> {
    audio.validate(&options.config)?;
    super::export_bundle_bytes_inner(program, options, Some((audio, assets)))
}

/// Record camera-rig motion and sound through their ordinary native drivers.
///
/// # Errors
/// Preserves camera export and soundtrack failures; a factory cannot pre-play.
pub fn export_camera_bundle_with_soundtrack_bytes<F, P>(
    factory: F,
    camera: CameraConfig,
    options: BundleExportOptions,
    audio: &BundleSoundtrackOptions,
    assets: &dyn FileSystem,
) -> Result<SceneBundleExport, BundleExportError>
where
    F: FnOnce(&mut Scene, &CameraConfig) -> crate::Result<(P, CameraRig)>,
    P: SceneConstruct,
{
    audio.validate(&options.config)?;
    super::camera::export_camera_bundle_bytes_inner(factory, camera, options, Some((audio, assets)))
}

/// Publish one native sound-bearing bundle with atomic no-clobber semantics.
/// Assets and output use the explicit filesystem, never ambient host reads.
///
/// # Errors
/// All scene, recording and audio work succeeds before private output staging.
pub fn export_bundle_with_soundtrack_with_fs<P: SceneConstruct + ?Sized>(
    program: &mut P,
    output: impl AsRef<Path>,
    options: BundleExportOptions,
    audio: &BundleSoundtrackOptions,
    fs: Arc<dyn FileSystem>,
) -> Result<BundleExportReport, BundleExportError> {
    let output = output.as_ref();
    validate_output(output)?;
    let export = export_bundle_with_soundtrack_bytes(program, options, audio, fs.as_ref())?;
    super::publish(export, output, fs)
}

/// Camera and sound counterpart of the create-only publication API.
///
/// # Errors
/// Preserves typed recording/audio/FS failures; an existing file is not replaced.
pub fn export_camera_bundle_with_soundtrack_with_fs<F, P>(
    factory: F,
    output: impl AsRef<Path>,
    camera: CameraConfig,
    options: BundleExportOptions,
    audio: &BundleSoundtrackOptions,
    fs: Arc<dyn FileSystem>,
) -> Result<BundleExportReport, BundleExportError>
where
    F: FnOnce(&mut Scene, &CameraConfig) -> crate::Result<(P, CameraRig)>,
    P: SceneConstruct,
{
    let output = output.as_ref();
    validate_output(output)?;
    let export =
        export_camera_bundle_with_soundtrack_bytes(factory, camera, options, audio, fs.as_ref())?;
    super::publish(export, output, fs)
}
fn validate_output(output: &Path) -> Result<(), BundleExportError> {
    if output.as_os_str().is_empty() || output.file_name().is_none() {
        Err(BundleExportError::InvalidOptions(
            "bundle output must name a file",
        ))
    } else {
        Ok(())
    }
}

pub(super) fn finish_recording(
    bundle: RecordedSceneBundle,
    scene: &Scene,
    options: &BundleExportOptions,
    audio: Option<(&BundleSoundtrackOptions, &dyn FileSystem)>,
) -> Result<(RecordedSceneBundle, Option<BundleSoundtrackReport>), BundleExportError> {
    if scene.sound_requests().is_empty() {
        return Ok((bundle, None));
    }
    let (audio, assets) = audio.ok_or(BundleExportError::Capability(
        "sound cues require explicit soundtrack export",
    ))?;
    let mut config = audio.mixer;
    // The native mix allocates f64 accumulation plus final PCM. Bound its
    // length before preparation, rather than discovering an oversized result
    // only after mixing. The envelope enforces the exact combined byte cap.
    let remaining = options
        .max_output_bytes
        .min(fmn_scene::DEFAULT_MAX_BUNDLE_BYTES)
        .saturating_sub(bundle.bytes.len());
    config.max_output_frames = config
        .max_output_frames
        .min((remaining / 4 / usize::from(config.channels)) as u64);
    let picture_frames = scene.time().frames().max(i64::from(bundle.frame_count));
    let minimum = frames_to_samples(picture_frames, scene.time().fps(), config.sample_rate)
        .map_err(|e| sound(RenderError::Sound(e)))?;
    let minimum = u64::try_from(minimum)
        .map_err(|_| BundleExportError::InvalidOptions("negative soundtrack timeline"))?;
    if minimum > config.max_output_frames {
        return Err(BundleExportError::InvalidOptions(
            "picture duration exceeds the soundtrack output budget",
        ));
    }
    let prepared = PreparedSceneSoundtrack::new_with_limits(
        assets,
        scene.sound_requests(),
        &mut audio.decoder(&options.config)?,
        config,
        None,
        audio.preparation,
    )
    .map_err(sound)?
    .with_timeline_frames(minimum);
    let mix = prepared.mix(audio.workers).map_err(sound)?;
    let report = BundleSoundtrackReport {
        mix: SoundtrackReport {
            cues_mixed: mix.cues_mixed,
            sample_frames: mix.audio.samples.len() as u64 / u64::from(mix.audio.channels),
            sample_rate: mix.audio.sample_rate,
            channels: mix.audio.channels,
            clipped_samples: mix.clipped_samples,
        },
        workers_used: mix.workers_used,
        inputs: prepared.inputs().to_vec(),
        audio_decodes: prepared.invocations(),
    };
    let pcm = BundleSoundtrack::new(mix.audio.sample_rate, mix.audio.channels, mix.audio.samples)
        .map_err(|e| sound(RenderError::Bundle(e)))?;
    let bundle = bundle
        .with_soundtrack(&pcm, options.max_output_bytes)
        .map_err(|e| sound(RenderError::Bundle(e)))?;
    Ok((bundle, Some(report)))
}
