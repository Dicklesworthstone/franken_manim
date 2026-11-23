//! Shared native-first source admission for WAV and portable soundtrack export.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use fmn_config::{Config, config::DeterminismMode};
use fmn_output::{
    AudioDecodeError, AudioDecodeLimits, AudioInputFormat, Boundary, FfmpegLimitsReport,
    FfmpegTool, JobLimits, ScopedAudioDecoder,
};
use fmn_platform::process::{FfmpegLocator, ProcessRunner};

use super::{FfmpegCapability, RenderError, SoundtrackPreparationLimits};

/// Source and process bounds shared by standalone WAV and FMTL soundtracks.
/// A native source never discovers or starts a tool. Recognized compressed
/// sources require an explicit capability and standard determinism mode.
#[derive(Clone, Debug, Default)]
pub struct SoundtrackDecoderOptions {
    /// Per-source byte and interleaved-sample ceilings.
    pub decode: AudioDecodeLimits,
    /// Cumulative storage admission; each source also respects these ceilings.
    pub preparation: SoundtrackPreparationLimits,
    /// Independent bounded policy for each governed decode job.
    pub ffmpeg_limits: JobLimits,
}

impl SoundtrackDecoderOptions {
    /// Construct a lazy decoder from an optional owned process capability.
    /// Neither validation nor native WAV decoding consults the capability.
    ///
    /// # Errors
    /// Invalid decode/process limits or executable configuration. Source and
    /// process refusals are deferred until the source is actually decoded.
    pub fn decoder<'a>(
        &self,
        config: &Config,
        capability: Option<&'a FfmpegCapability>,
    ) -> Result<ScopedAudioDecoder<'a>, RenderError> {
        match capability {
            Some(capability) => self.decoder_with_capabilities(
                config,
                Arc::clone(&capability.runner),
                capability.locator.as_ref(),
                capability.workdir_root.clone(),
            ),
            None => self.with_resolver(config, false, |format, _, _| {
                Err(AudioDecodeError::TranscoderRequired { format })
            }),
        }
    }

    /// Borrow an injected locator while retaining the caller's exact runner.
    /// This is the CLI counterpart of [`Self::decoder`], with the same lazy
    /// resolution, certified refusal, executable identity and resource policy.
    ///
    /// # Errors
    /// The same typed configuration and source refusals as [`Self::decoder`].
    pub fn decoder_with_capabilities<'a>(
        &self,
        config: &Config,
        runner: Arc<dyn ProcessRunner>,
        locator: &'a dyn FfmpegLocator,
        workdir_root: PathBuf,
    ) -> Result<ScopedAudioDecoder<'a>, RenderError> {
        self.with_resolver(config, true, move |_, executable, limits| {
            let located = locator
                .locate_ffmpeg(executable)
                .map_err(|error| AudioDecodeError::Capability(error.to_string()))?;
            let tool = FfmpegTool::resolve(located, runner.as_ref(), &workdir_root)?;
            Boundary::new(
                tool,
                Arc::clone(&runner),
                limits.clone(),
                workdir_root.clone(),
            )
            .map_err(AudioDecodeError::Boundary)
        })
    }

    /// Effective source limits after cumulative storage admission.
    #[must_use]
    pub fn effective_decode_limits(&self) -> AudioDecodeLimits {
        AudioDecodeLimits {
            max_input_bytes: self
                .decode
                .max_input_bytes
                .min(super::soundtrack::MAX_SOUND_ASSET_BYTES as u64)
                .min(self.preparation.max_source_bytes as u64),
            max_samples: self
                .decode
                .max_samples
                .min(self.preparation.max_decoded_samples as u64),
        }
    }

    /// Admitted external policy, including the independently enforced source
    /// byte ceiling. Individual decodes further tighten output to remaining
    /// sample storage, as enforced by the shared audio decoder.
    ///
    /// # Errors
    /// Invalid job limits, without resolving or starting a tool.
    pub fn limits_report(&self) -> Result<FfmpegLimitsReport, RenderError> {
        let limits = self.admitted_job_limits()?;
        Ok(FfmpegLimitsReport::new(
            &limits,
            self.effective_decode_limits().max_input_bytes,
        ))
    }

    fn admitted_job_limits(&self) -> Result<JobLimits, RenderError> {
        fmn_output::admit_render_ffmpeg_limits(
            &self.ffmpeg_limits,
            self.ffmpeg_limits.max_artifact_bytes,
        )
        .map_err(|error| decode_error(AudioDecodeError::InvalidJobLimits(error)))
    }

    fn with_resolver<'a>(
        &self,
        config: &Config,
        has_capability: bool,
        mut resolve: impl FnMut(
            AudioInputFormat,
            &Path,
            &JobLimits,
        ) -> Result<Boundary, AudioDecodeError>
        + Send
        + Sync
        + 'a,
    ) -> Result<ScopedAudioDecoder<'a>, RenderError> {
        let limits = self.effective_decode_limits();
        let jobs = self.admitted_job_limits()?;
        let executable = config.file_writer.ffmpeg_bin.clone();
        if has_capability && (executable.is_empty() || executable.contains('\0')) {
            return Err(RenderError::InvalidOptions(
                "soundtrack ffmpeg executable must not be empty or contain NUL",
            ));
        }
        let certified = config.determinism.mode == DeterminismMode::Certified;
        ScopedAudioDecoder::with_boundary_resolver(limits, move |format| {
            if certified {
                return Err(AudioDecodeError::Capability(format!(
                    "{} decoding through ffmpeg is outside certified soundtrack preparation; supply native PCM WAV",
                    format.demuxer(),
                )));
            }
            resolve(format, Path::new(&executable), &jobs)
        })
        .map_err(decode_error)
    }
}

fn decode_error(error: AudioDecodeError) -> RenderError {
    RenderError::SoundDecode {
        source: None,
        error,
    }
}
