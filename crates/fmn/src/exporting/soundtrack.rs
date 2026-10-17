//! Explicit native audio export: one real scene run, one shared mix, one bundle.
//!
//! Asset access is a caller capability. This entry point supports native WAV;
//! it never starts a transcoder or stores a path/callback in the artifact.
use super::{BundleExportError, BundleExportOptions, BundleExportReport, SceneBundleExport};
use crate::rendering::{PreparedSceneSoundtrack, SoundtrackPreparationLimits};
use crate::{SceneConstruct, rendering::RenderError};
use fmn_output::{AudioDecodeLimits, AudioDecoder, MixerConfig, SoundMixer, frames_to_samples};
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
    /// Per-source native WAV decoding bounds; no transcoder capability is added.
    pub decode: AudioDecodeLimits,
    /// Cumulative source, decoded and per-cue prepared-storage admission.
    pub preparation: SoundtrackPreparationLimits,
    /// Native mixer workers, validated before authored scene execution.
    pub workers: usize,
}
impl Default for BundleSoundtrackOptions {
    fn default() -> Self {
        Self {
            mixer: MixerConfig::default(),
            decode: AudioDecodeLimits::default(),
            preparation: SoundtrackPreparationLimits::default(),
            workers: 1,
        }
    }
}
impl BundleSoundtrackOptions {
    fn validate(&self) -> Result<(), BundleExportError> {
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
        self.decoder()?;
        Ok(())
    }
    fn decoder(&self) -> Result<AudioDecoder, BundleExportError> {
        let mut limits = self.decode.clone();
        limits.max_input_bytes = limits
            .max_input_bytes
            .min(self.preparation.max_source_bytes as u64);
        limits.max_samples = limits
            .max_samples
            .min(self.preparation.max_decoded_samples as u64);
        AudioDecoder::new(limits, None).map_err(|error| {
            sound(RenderError::SoundDecode {
                path: Default::default(),
                error,
            })
        })
    }
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
    audio.validate()?;
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
    audio.validate()?;
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
) -> Result<RecordedSceneBundle, BundleExportError> {
    if scene.sound_requests().is_empty() {
        return Ok(bundle);
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
        &mut audio.decoder()?,
        config,
        None,
        audio.preparation,
    )
    .map_err(sound)?
    .with_timeline_frames(minimum);
    let mix = prepared.mix(audio.workers).map_err(sound)?;
    let pcm = BundleSoundtrack::new(mix.audio.sample_rate, mix.audio.channels, mix.audio.samples)
        .map_err(|e| sound(RenderError::Bundle(e)))?;
    bundle
        .with_soundtrack(&pcm, options.max_output_bytes)
        .map_err(|e| sound(RenderError::Bundle(e)))
}
