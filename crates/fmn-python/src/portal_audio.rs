//! Scene.add_sound → native sound assets → Reel mix, with decode receipts.
//!
//! The portal owns no decoder, resampler, process, or audio publication path.
//! Capture the host configuration before authored lifecycle callbacks, then
//! consume the same rational cue requests as the native scene runtime.

use super::*;
use fmn_output::{AudioDecodeError, AudioDecodeLimits, AudioDecodeReport, AudioDecoder};

pub(crate) struct PortalAudioInput {
    path: PathBuf,
    report: Arc<AudioDecodeReport>,
    channels: u16,
    sample_rate: u32,
    sample_frames: u64,
}

pub(crate) struct PortalAudio {
    decoder: AudioDecoder,
    first_request: usize,
    scene_window: bool,
    minimum_window_frames: i64,
    pub(crate) inputs: Vec<PortalAudioInput>,
    pub(crate) ffmpeg_limits: Option<fmn_output::FfmpegLimitsReport>,
}

impl PortalAudio {
    pub(crate) fn from_scene(
        scene: &Bound<'_, PyScene>,
        format: &PortalOutputFormat,
        video: Option<&portal_video::PortalVideoConfig>,
    ) -> PyResult<Box<Self>> {
        let (executable, policy) = if let Some(video) = video {
            (
                Some(video.ffmpeg_bin.clone()),
                Some((video.job_limits.clone(), video.max_input_bytes)),
            )
        } else if matches!(format, PortalOutputFormat::Wav) {
            let writer = scene.getattr("file_writer")?;
            (
                Some(writer.getattr("ffmpeg_bin")?.extract::<String>()?),
                Some(portal_video::limits_from_writer(
                    &writer,
                    AudioDecodeLimits::default().max_input_bytes,
                )?),
            )
        } else {
            (None, None)
        };
        if executable
            .as_ref()
            .is_some_and(|value| value.is_empty() || value.contains('\0'))
        {
            return Err(PyValueError::new_err(
                "file_writer.ffmpeg_bin must be nonempty and contain no NUL",
            ));
        }
        let mut decode_limits = AudioDecodeLimits::default();
        if let Some((_, max_input_bytes)) = &policy {
            decode_limits.max_input_bytes = decode_limits.max_input_bytes.min(*max_input_bytes);
        }
        let mut decoder = AudioDecoder::new(decode_limits, executable.map(PathBuf::from))
            .map_err(decode_error)?;
        let ffmpeg_limits = policy.map(|(limits, max_input_bytes)| {
            let report = fmn_output::FfmpegLimitsReport::new(&limits, max_input_bytes);
            (limits, report)
        });
        if let Some((limits, _)) = &ffmpeg_limits {
            decoder = decoder
                .with_job_limits(limits.clone())
                .map_err(decode_error)?;
        }
        Ok(Box::new(Self {
            decoder,
            first_request: 0,
            scene_window: false,
            minimum_window_frames: i64::from(video.is_some()),
            inputs: Vec::new(),
            ffmpeg_limits: ffmpeg_limits.map(|(_, report)| report),
        }))
    }

    /// A live recording includes only cues authored after it acquired output.
    pub(crate) fn begin_at_request(&mut self, first_request: usize) {
        self.first_request = first_request;
    }

    /// Subdivided clips hear all currently authored cues, cropped on the
    /// absolute scene-sample timeline. This is distinct from live inserts,
    /// which intentionally include only cues added after recording starts.
    pub(crate) fn use_scene_window(&mut self) {
        self.first_request = 0;
        self.scene_window = true;
    }

    pub(crate) fn mix(
        &mut self,
        scene: &Scene,
        threads: usize,
        timeline: &OutputTimeline,
    ) -> PyResult<Option<fmn_output::MixReport>> {
        let requests = scene
            .sound_requests()
            .get(self.first_request..)
            .ok_or_else(|| {
                PyRuntimeError::new_err(
                    "recording sound history was truncated; cancel before restoring scene state",
                )
            })?;
        if requests.is_empty() && !self.scene_window {
            return Ok(None);
        }
        let config = fmn_output::MixerConfig::default();
        let time = scene.time();
        let window = if self.scene_window {
            let (start, end) = timeline.scene_window()?;
            let end = end.max(
                start
                    .checked_add(self.minimum_window_frames)
                    .ok_or_else(|| PyOverflowError::new_err("audio window overflow"))?,
            );
            let start = fmn_output::frames_to_samples(start, time.fps(), config.sample_rate)
                .map_err(native_error)?;
            let end = fmn_output::frames_to_samples(end, time.fps(), config.sample_rate)
                .map_err(native_error)?;
            if end < start || end as u64 > config.max_output_frames {
                return Err(PyValueError::new_err(
                    "audio window exceeds the native timeline budget",
                ));
            }
            Some((start as u64, (end - start) as u64))
        } else {
            None
        };
        let timeline_frames = fmn_output::frames_to_samples(
            timeline.output_frame(time.frames())?,
            time.fps(),
            config.sample_rate,
        )
        .map_err(native_error)?;
        let mut mixer = fmn_output::SoundMixer::new(config)
            .map_err(native_error)?
            .with_timeline_frames(u64::try_from(timeline_frames).map_err(native_error)?);
        self.inputs.clear();
        let fs = fmn_platform::fs::StdFs;
        let max_input_bytes = self
            .ffmpeg_limits
            .as_ref()
            .map_or(64 * 1024 * 1024, |limits| {
                limits.max_input_bytes.min(64 * 1024 * 1024) as usize
            });
        for request in requests {
            let bytes = fmn_platform::fs::FileSystem::read_bounded(
                &fs,
                &request.sound_file,
                max_input_bytes,
            )
            .map_err(|error| {
                PyOSError::new_err(format!(
                    "sound cue {}: {error}",
                    request.sound_file.display()
                ))
            })?;
            let decoded = self.decoder.decode(&bytes).map_err(|error| {
                let message = format!("sound cue {}: {error}", request.sound_file.display());
                decode_message(&error, message)
            })?;
            let input = PortalAudioInput {
                path: request.sound_file.clone(),
                channels: decoded.audio.channels,
                sample_rate: decoded.audio.sample_rate,
                sample_frames: (decoded.audio.samples.len() / usize::from(decoded.audio.channels))
                    as u64,
                report: Arc::new(decoded.report),
            };
            mixer
                .add(fmn_output::SoundCue {
                    audio: decoded.audio,
                    frame: if window.is_some() {
                        request.time.frames()
                    } else {
                        timeline.output_frame(request.time.frames())?
                    },
                    fps: request.time.fps(),
                    time_offset: request.time_offset,
                    gain: request.gain,
                    gain_to_background: request.gain_to_background,
                })
                .map_err(native_error)?;
            self.inputs.push(input);
        }
        match window {
            Some((start, frames)) => mixer.mix_window(start, frames, threads),
            None => mixer.mix(threads),
        }
        .map(Some)
        .map_err(native_error)
    }

    pub(crate) fn invocations(&self) -> Vec<fmn_output::InvocationReport> {
        self.inputs
            .iter()
            .filter_map(|input| input.report.invocation.clone())
            .collect()
    }
}

fn decode_error(error: AudioDecodeError) -> PyErr {
    decode_message(&error, error.to_string())
}

fn decode_message(error: &AudioDecodeError, message: String) -> PyErr {
    match error {
        AudioDecodeError::Capability(_) | AudioDecodeError::TranscoderRequired { .. } => {
            CapabilityError::new_err(message)
        }
        AudioDecodeError::Boundary(_) => PyRuntimeError::new_err(message),
        _ => PyValueError::new_err(message),
    }
}

pub(crate) fn input_facts(scene: &Bound<'_, PyScene>) -> PyResult<Py<PyList>> {
    let facts = PyList::empty(scene.py());
    for input in &scene.borrow().render_audio_inputs {
        let fact = PyDict::new(scene.py());
        fact.set_item("path", input.path.to_string_lossy().as_ref())?;
        fact.set_item("source_sha256", input.report.source_digest.to_hex())?;
        fact.set_item("pcm_sha256", input.report.pcm_digest.to_hex())?;
        fact.set_item("container", input.report.format.demuxer())?;
        fact.set_item("sample_rate", input.sample_rate)?;
        fact.set_item("channels", input.channels)?;
        fact.set_item("sample_frames", input.sample_frames)?;
        fact.set_item(
            "decoder",
            if input.report.invocation.is_some() {
                "ffmpeg"
            } else {
                "native-wav"
            },
        )?;
        fact.set_item(
            "decoder_sha256",
            input
                .report
                .invocation
                .as_ref()
                .map(|invocation| &invocation.provenance.tool_sha256_hex),
        )?;
        facts.append(fact)?;
    }
    Ok(facts.unbind())
}

/// Portable export policy. Each byte cap bounds one preparation pool, not
/// process RSS; the final container has its separate combined cap.
#[derive(Clone, Copy)]
pub(crate) struct BundleAudioOptions {
    pub(crate) sample_rate: u32,
    pub(crate) channels: u16,
    pub(crate) workers: usize,
    pub(crate) max_bytes: usize,
    pub(crate) allow_ffmpeg: bool,
}

/// One export owns a frozen decoder configuration and retained tool identity.
pub(crate) struct BundleAudioCapture {
    options: BundleAudioOptions,
    decoder: AudioDecoder,
    ffmpeg_limits: Option<fmn_output::FfmpegLimitsReport>,
}

pub(crate) struct BundleAudioMix {
    pub(crate) soundtrack: fmn_scene::timeline_bundle::BundleSoundtrack,
    pub(crate) inputs: Vec<PortalAudioInput>,
    pub(crate) invocations: Vec<fmn_output::InvocationReport>,
    pub(crate) ffmpeg_limits: Option<fmn_output::FfmpegLimitsReport>,
}

impl BundleAudioOptions {
    pub(crate) fn validate(self) -> PyResult<Self> {
        if !(1..=384_000).contains(&self.sample_rate)
            || !matches!(self.channels, 1 | 2)
            || !(1..=64).contains(&self.workers)
            || !(8..=fmn_hash::serial::Limits::DEFAULT.max_total).contains(&self.max_bytes)
        {
            return Err(PyValueError::new_err(
                "bundle audio requires rate 1..384000, channels 1 or 2, workers 1..64 and 8..256 MiB bytes",
            ));
        }
        Ok(self)
    }

    /// Snapshot authored writer settings before acquiring the scene generation.
    /// The existing lazy decoder captures PATH/current directory/temp root;
    /// no executable discovery or process runs until a compressed cue needs it.
    pub(crate) fn capture(self, scene: &Bound<'_, PyScene>) -> PyResult<BundleAudioCapture> {
        self.validate()?;
        let mut limits = AudioDecodeLimits {
            max_input_bytes: self.max_bytes.min(64 * 1024 * 1024) as u64,
            max_samples: (self.max_bytes / 4) as u64,
        };
        let (executable, policy) = if self.allow_ffmpeg {
            let writer = scene.getattr("file_writer")?;
            let executable = writer.getattr("ffmpeg_bin")?.extract::<String>()?;
            if executable.is_empty() || executable.contains('\0') {
                return Err(PyValueError::new_err(
                    "file_writer.ffmpeg_bin must be nonempty and contain no NUL",
                ));
            }
            let (policy, input_limit) =
                portal_video::limits_from_writer(&writer, limits.max_input_bytes)?;
            limits.max_input_bytes = limits.max_input_bytes.min(input_limit);
            (Some(PathBuf::from(executable)), Some(policy))
        } else {
            (None, None)
        };
        let ffmpeg_limits = policy
            .as_ref()
            .map(|policy| fmn_output::FfmpegLimitsReport::new(policy, limits.max_input_bytes));
        let mut decoder = AudioDecoder::new(limits, executable).map_err(decode_error)?;
        if let Some(policy) = policy {
            decoder = decoder.with_job_limits(policy).map_err(decode_error)?;
        }
        Ok(BundleAudioCapture {
            options: self,
            decoder,
            ffmpeg_limits,
        })
    }
}

impl BundleAudioCapture {
    /// Compose the real Scene requests through Reel's decoder and mixer.
    /// No Python descriptor or alternate resampler runs here. Repeated paths
    /// are read and decoded once; configured transcoding uses the same frozen
    /// native capability throughout this export generation.
    pub(crate) fn mix(
        mut self,
        scene: &Scene,
        picture_frames: u32,
        remaining_bytes: usize,
    ) -> PyResult<Option<BundleAudioMix>> {
        use fmn_output::{MixerConfig, SoundCue, SoundMixer, frames_to_samples};
        use fmn_platform::fs::{FileSystem, StdFs};
        use std::collections::{BTreeMap, btree_map::Entry};
        let options = self.options.validate()?;
        let requests = scene.sound_requests();
        if requests.is_empty() {
            return Ok(None);
        }
        if requests.len() > 4096 {
            return Err(PyRuntimeError::new_err(
                "bundle soundtrack exceeds 4096 cues",
            ));
        }
        let time = scene.time();
        // Explicit show() adds pictures without advancing the Scene clock.
        // Refuse that unmodelled time mapping rather than shifting later cues.
        // A frameless program's one terminal still is the only exception.
        if i64::from(picture_frames) != time.frames()
            && !(picture_frames == 1 && time.frames() == 0)
        {
            return Err(CapabilityError::new_err(
                "soundtrack export cannot retime explicit show captures; use play/wait or a terminal still",
            ));
        }
        let config = MixerConfig {
            sample_rate: options.sample_rate,
            channels: options.channels,
            max_output_frames: (remaining_bytes.min(options.max_bytes)
                / 4
                / usize::from(options.channels)) as u64,
        };
        let minimum =
            frames_to_samples(i64::from(picture_frames), time.fps(), options.sample_rate)
                .map_err(native_error)?;
        let minimum = u64::try_from(minimum).map_err(native_error)?;
        if config.max_output_frames == 0 || minimum > config.max_output_frames {
            return Err(PyRuntimeError::new_err(
                "bundle soundtrack output budget exhausted",
            ));
        }
        let mut mixer = SoundMixer::new(config)
            .map_err(native_error)?
            .with_timeline_frames(minimum);
        let mut cache = BTreeMap::new();
        let mut source_bytes = 0;
        let mut decoded_bytes = 0;
        let mut prepared_bytes = 0;
        let mut inputs = Vec::new();
        let mut invocations = Vec::new();
        for request in requests {
            let (audio, report) = match cache.entry(request.sound_file.clone()) {
                Entry::Occupied(entry) => entry.into_mut(),
                Entry::Vacant(entry) => {
                    let raw_budget = (options.max_bytes - source_bytes)
                        .min(64 * 1024 * 1024)
                        .min(self.ffmpeg_limits.as_ref().map_or(usize::MAX, |policy| {
                            policy.max_input_bytes as usize
                        }));
                    let sample_budget = (options.max_bytes - decoded_bytes) / 4;
                    if raw_budget == 0 || sample_budget == 0 {
                        return Err(PyRuntimeError::new_err(
                            "bundle soundtrack source/decode budget exhausted",
                        ));
                    }
                    let bytes = StdFs
                        .read_bounded(&request.sound_file, raw_budget)
                        .map_err(|e| {
                            PyOSError::new_err(format!(
                                "sound cue {}: {e}",
                                request.sound_file.display()
                            ))
                        })?;
                    source_bytes += bytes.len();
                    let decoded = self
                        .decoder
                        .decode_with_limits(
                            &bytes,
                            &AudioDecodeLimits {
                                max_input_bytes: raw_budget as u64,
                                max_samples: sample_budget as u64,
                            },
                        )
                        .map_err(|e| {
                            decode_message(
                                &e,
                                format!("sound cue {}: {e}", request.sound_file.display()),
                            )
                        })?;
                    decoded_bytes += decoded.audio.samples.len() * 4;
                    let report = Arc::new(decoded.report);
                    if let Some(invocation) = &report.invocation {
                        invocations.push(invocation.clone());
                    }
                    entry.insert((decoded.audio, report))
                }
            };
            if options.allow_ffmpeg {
                inputs.push(PortalAudioInput {
                    path: request.sound_file.clone(),
                    channels: audio.channels,
                    sample_rate: audio.sample_rate,
                    sample_frames: (audio.samples.len() / usize::from(audio.channels)) as u64,
                    report: Arc::clone(report),
                });
            }
            // Admission arithmetic only; Reel owns mapping/resampling and
            // placement. Charge each retained f64 cue before cloning its PCM.
            let frames = audio.samples.len() / usize::from(audio.channels);
            let mapped = (frames as u128) * u128::from(options.channels) * 8;
            let prepared = ((frames as u128) * u128::from(options.sample_rate))
                .div_ceil(u128::from(audio.sample_rate))
                * u128::from(options.channels)
                * 8;
            if mapped > options.max_bytes as u128
                || prepared > (options.max_bytes - prepared_bytes) as u128
            {
                return Err(PyRuntimeError::new_err(
                    "bundle soundtrack prepared-sample budget exhausted",
                ));
            }
            prepared_bytes += prepared as usize;
            mixer
                .add(SoundCue {
                    audio: audio.clone(),
                    frame: request.time.frames(),
                    fps: request.time.fps(),
                    time_offset: request.time_offset,
                    gain: request.gain,
                    gain_to_background: request.gain_to_background,
                })
                .map_err(native_error)?;
        }
        let mixed = mixer.mix(options.workers).map_err(native_error)?;
        let soundtrack = fmn_scene::timeline_bundle::BundleSoundtrack::new(
            mixed.audio.sample_rate,
            mixed.audio.channels,
            mixed.audio.samples,
        )
        .map_err(native_error)?;
        Ok(Some(BundleAudioMix {
            soundtrack,
            inputs,
            invocations,
            ffmpeg_limits: self.ffmpeg_limits,
        }))
    }
}

/// Verify report sharing after a real repeated-source bundle export.
#[cfg(any(test, feature = "gauntlet"))]
pub(crate) fn shared_bundle_input_reports(scene: &Bound<'_, PyScene>) -> PyResult<bool> {
    let owner = scene.try_borrow()?;
    let inputs = &owner.render_audio_inputs;
    Ok(inputs.len() == 2 && Arc::ptr_eq(&inputs[0].report, &inputs[1].report))
}

#[cfg(test)]
mod tests {
    use pyo3::types::PyDictMethods as _;
    #[test]
    fn portal_compressed_audio_acceptance() {
        crate::with_python_test_module("compressed audio", |py, _module, globals| {
            globals
                .set_item(
                    "_audio_fixture_generator",
                    concat!(
                        env!("CARGO_MANIFEST_DIR"),
                        "/../fmn-output/tests/make_audio_fixtures.py"
                    ),
                )
                .unwrap();
            let source = std::ffi::CString::new(include_str!("../tests/audio_inputs.py")).unwrap();
            py.run(source.as_c_str(), Some(globals), Some(globals))
                .inspect_err(|error| error.print(py))
                .expect("real compressed sound and output receipts");
        });
    }
}
