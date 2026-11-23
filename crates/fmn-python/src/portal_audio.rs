//! Scene.add_sound → native sound assets → Reel mix, with decode receipts.
//!
//! The portal owns no decoder, resampler, process, or audio publication path.
//! Capture the host configuration before authored lifecycle callbacks, then
//! consume the same rational cue requests as the native scene runtime.

use super::*;
use fmn_output::{
    AudioDecodeError, AudioDecodeLimits, AudioDecodeReport, AudioDecoder, DitherPolicy,
};
use fmn_scene::{SoundData, SoundDataLimits, SoundSource, SoundTime};
use pyo3::exceptions::PyAttributeError;
use pyo3::types::{PyBool, PyBytes};
use std::borrow::Cow;

/// Immutable, owned PCM. Construction admits the complete size before reading
/// samples, and native SoundData owns canonicalization and content identity.
#[pyclass(frozen, skip_from_py_object, name = "SoundData", module = "fmn_python")]
#[derive(Clone)]
pub(crate) struct PySoundData {
    data: SoundData,
}

#[pymethods]
impl PySoundData {
    /// Freeze a finite, indexable sequence of interleaved mono/stereo samples.
    /// Samples are normalized floating point PCM; amplitudes outside [-1, 1]
    /// remain available to the native mixer's gain and clipping policy.
    #[new]
    #[pyo3(signature = (samples, sample_rate=48000, channels=1, *, max_samples=16_777_216, max_bytes=67_108_864))]
    fn new(
        samples: &Bound<'_, PyAny>,
        sample_rate: u32,
        channels: u16,
        max_samples: usize,
        max_bytes: usize,
    ) -> PyResult<Self> {
        let limits = sound_data_limits(max_samples, max_bytes)?;
        if !matches!(channels, 1 | 2)
            || sample_rate == 0
            || sample_rate.checked_mul(u32::from(channels) * 4).is_none()
        {
            return Err(PyValueError::new_err(
                "SoundData requires mono/stereo channels and a valid nonzero sample rate",
            ));
        }
        let count = samples.len().map_err(|_| {
            PyTypeError::new_err(
                "SoundData samples must be a finite, indexable sequence of interleaved numbers",
            )
        })?;
        if count > limits.max_samples
            || count
                .checked_mul(4)
                .and_then(|n| n.checked_add(44))
                .is_none_or(|bytes| bytes > limits.max_bytes)
        {
            return Err(PyValueError::new_err(
                "SoundData samples exceed the admitted PCM budget",
            ));
        }
        if !count.is_multiple_of(usize::from(channels)) {
            return Err(PyValueError::new_err(
                "SoundData has an incomplete interleaved sample frame",
            ));
        }
        let mut values = Vec::new();
        values.try_reserve_exact(count).map_err(|_| {
            pyo3::exceptions::PyMemoryError::new_err("SoundData sample allocation failed")
        })?;
        for index in 0..count {
            let sample = samples.get_item(index)?;
            if sample.is_instance_of::<PyBool>() {
                return Err(PyTypeError::new_err(
                    "SoundData samples must be numbers, not bool",
                ));
            }
            let value = sample.extract::<f64>()? as f32;
            if !value.is_finite() {
                return Err(PyValueError::new_err(
                    "SoundData samples must be finite f32 values",
                ));
            }
            values.push(value);
        }
        let data = SoundData::from_audio(
            &fmn_codec::WavAudio {
                channels,
                sample_rate,
                format: fmn_codec::SampleFormat::F32,
                samples: values,
            },
            limits,
        )
        .map_err(native_error)?;
        Ok(Self { data })
    }

    /// Decode owned PCM WAV bytes without a file, decoder process or tool lookup.
    #[staticmethod]
    #[pyo3(signature = (data, *, max_samples=16_777_216, max_bytes=67_108_864))]
    fn from_wav_bytes(data: &[u8], max_samples: usize, max_bytes: usize) -> PyResult<Self> {
        let limits = sound_data_limits(max_samples, max_bytes)?;
        Ok(Self {
            data: SoundData::from_wav_bytes(data, limits).map_err(native_error)?,
        })
    }

    #[getter]
    fn channels(&self) -> u16 {
        self.data.channels()
    }

    #[getter]
    fn sample_rate(&self) -> u32 {
        self.data.sample_rate()
    }

    #[getter]
    fn sample_frames(&self) -> usize {
        self.data.sample_frames()
    }

    #[getter]
    fn duration_seconds(&self) -> f64 {
        self.data.sample_frames() as f64 / f64::from(self.data.sample_rate())
    }

    #[getter]
    fn sha256(&self) -> String {
        self.data.digest().to_hex()
    }

    /// Return independent immutable canonical F32 WAV bytes.
    fn wav_bytes<'py>(&self, py: Python<'py>) -> Bound<'py, PyBytes> {
        PyBytes::new(py, self.data.bytes())
    }

    fn __copy__(&self) -> Self {
        self.clone()
    }

    fn __deepcopy__(&self, _memo: &Bound<'_, PyAny>) -> Self {
        self.clone()
    }

    fn __repr__(&self) -> String {
        format!(
            "SoundData(sample_rate={}, channels={}, sample_frames={}, sha256='{}')",
            self.data.sample_rate(),
            self.data.channels(),
            self.data.sample_frames(),
            self.data.digest()
        )
    }
}

fn sound_data_limits(max_samples: usize, max_bytes: usize) -> PyResult<SoundDataLimits> {
    let maximum = SoundDataLimits::default();
    if max_samples > maximum.max_samples || !(44..=maximum.max_bytes).contains(&max_bytes) {
        return Err(PyValueError::new_err(
            "SoundData limits require at most 16777216 samples and 44..67108864 bytes",
        ));
    }
    Ok(SoundDataLimits {
        max_samples,
        max_bytes,
    })
}

fn absolute_sample_position(
    time: SoundTime,
    sample_rate: u32,
) -> Result<i64, fmn_output::SoundError> {
    match time {
        SoundTime::Seconds(seconds) => fmn_output::seconds_to_samples(seconds, sample_rate),
        SoundTime::Rational(position) => {
            fmn_output::frames_to_samples(position.frames(), position.fps(), sample_rate)
        }
    }
}

/// Resolve authored paths at the same boundary as file-only Scene.add_sound.
/// An immutable memory source never invokes a Python path hook or filesystem.
pub(crate) fn source_from_python(
    scene: &Bound<'_, PyScene>,
    value: &Bound<'_, PyAny>,
) -> PyResult<SoundSource> {
    if let Ok(data) = value.extract::<PyRef<'_, PySoundData>>() {
        return Ok(SoundSource::Memory(data.data.clone()));
    }
    let py = scene.py();
    let path = py
        .import("os")?
        .getattr("fspath")?
        .call1((value,))?
        .extract::<String>()?;
    let found = py
        .import("manimlib.utils.sounds")?
        .getattr("get_full_sound_file_path")?
        .call1((path.as_str(),));
    let mut path = match found {
        Ok(path) => path.extract::<PathBuf>()?,
        Err(error) if error.is_instance_of::<PyOSError>(py) => PathBuf::from(path),
        Err(error) => return Err(error),
    };
    let active = scene
        .borrow()
        .render
        .lock()
        .map_err(|_| PyRuntimeError::new_err("portal render session lock was poisoned"))?
        .is_some();
    if active && path.is_relative() {
        path = std::env::current_dir()
            .map_err(|error| PyOSError::new_err(error.to_string()))?
            .join(path);
    }
    Ok(SoundSource::File(path))
}

/// Observe authored cues through the production composer. No output generation
/// is acquired and no scene callback or clock step is run. The returned PCM
/// snapshot cannot alias either the scene queue or caller-owned sample arrays.
pub(crate) fn writer_snapshot(
    scene: &Bound<'_, PyScene>,
    writer: &Bound<'_, PyAny>,
) -> PyResult<PySoundData> {
    let mut audio = PortalAudio::from_writer(Some(writer), &PortalOutputFormat::Wav, None)?;
    let engine = Rc::clone(&scene.borrow().engine);
    let requests = {
        let scene = engine.scene.try_borrow().map_err(|_| {
            PyRuntimeError::new_err("audio snapshot cannot read a mutably borrowed Scene")
        })?;
        if scene.sound_requests().len() > 4096 {
            return Err(PyValueError::new_err("audio snapshot exceeds 4096 cues"));
        }
        scene.sound_requests().to_vec()
    };
    let limits = SoundDataLimits::default();
    let config = fmn_output::MixerConfig {
        max_output_frames: (limits.max_bytes.saturating_sub(44) / 8) as u64,
        ..fmn_output::MixerConfig::default()
    };
    let preparation = fmn::rendering::SoundtrackPreparationLimits {
        max_cues: 4096,
        max_source_bytes: limits.max_bytes,
        max_decoded_samples: limits.max_samples,
        max_prepared_samples: limits.max_samples,
    };
    let mixed = scene
        .py()
        .detach(move || {
            fmn::rendering::PreparedSceneSoundtrack::new_with_limits(
                &fmn_platform::fs::StdFs,
                &requests,
                &mut audio.decoder,
                config,
                None,
                preparation,
            )?
            .mix(1)
        })
        .map_err(native_error)?;
    Ok(PySoundData {
        data: SoundData::from_audio(&mixed.audio, limits).map_err(native_error)?,
    })
}

/// Appending needs only the native soundtrack's end. In particular, a valid
/// long low-rate source must not allocate an upsampled stereo snapshot merely
/// to determine where its successor starts.
pub(crate) fn writer_duration(
    scene: &Bound<'_, PyScene>,
    writer: &Bound<'_, PyAny>,
) -> PyResult<f64> {
    Ok(writer_end(scene, writer)?.to_f64())
}

/// Preserve the canonical writer sample grid when scheduling an append.
/// Floating-point seconds are an observation API, not an intermediate clock.
pub(crate) fn writer_end(
    scene: &Bound<'_, PyScene>,
    writer: &Bound<'_, PyAny>,
) -> PyResult<fmn_anim::RationalTime> {
    let mut audio = PortalAudio::from_writer(Some(writer), &PortalOutputFormat::Wav, None)?;
    let engine = Rc::clone(&scene.borrow().engine);
    let requests = {
        let scene = engine.scene.try_borrow().map_err(|_| {
            PyRuntimeError::new_err("audio duration cannot read a mutably borrowed Scene")
        })?;
        if scene.sound_requests().len() > 4096 {
            return Err(PyValueError::new_err("audio duration exceeds 4096 cues"));
        }
        scene.sound_requests().to_vec()
    };
    let config = fmn_output::MixerConfig::default();
    let frames = scene
        .py()
        .detach(move || {
            fmn::rendering::PreparedSceneSoundtrack::end_frame(
                &fmn_platform::fs::StdFs,
                &requests,
                &mut audio.decoder,
                config,
                4096,
            )
        })
        .map_err(native_error)?;
    Ok(fmn_anim::RationalTime::zero(config.sample_rate)
        + i64::try_from(frames).map_err(native_error)?)
}

pub(crate) struct PortalAudioInput {
    source: SoundSource,
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
    pub(crate) dither: DitherPolicy,
}

impl PortalAudio {
    pub(crate) fn from_scene(
        scene: &Bound<'_, PyScene>,
        format: &PortalOutputFormat,
        video: Option<&portal_video::PortalVideoConfig>,
    ) -> PyResult<Box<Self>> {
        // Freeze before setup/construct/tear_down. Publication must never
        // reinterpret a writer setting changed by authored scene callbacks.
        // Bare native PyScene instances need no writer for pixel-only output.
        let writer = match scene.getattr("file_writer") {
            Ok(writer) => Some(writer),
            Err(error) if error.is_instance_of::<PyAttributeError>(scene.py()) => None,
            Err(error) => return Err(error),
        };
        Self::from_writer(writer.as_ref(), format, video)
    }

    fn from_writer(
        writer: Option<&Bound<'_, PyAny>>,
        format: &PortalOutputFormat,
        video: Option<&portal_video::PortalVideoConfig>,
    ) -> PyResult<Box<Self>> {
        let dither = match &writer {
            Some(writer) => dither_from_writer(writer, format)?,
            None => DitherPolicy::None,
        };
        let (executable, policy) = if let Some(video) = video {
            (
                Some(video.ffmpeg_bin.clone()),
                Some((video.job_limits.clone(), video.max_input_bytes)),
            )
        } else if matches!(format, PortalOutputFormat::Wav) {
            let writer = writer.as_ref().ok_or_else(|| {
                PyAttributeError::new_err("WAV output requires Scene.file_writer")
            })?;
            (
                Some(writer.getattr("ffmpeg_bin")?.extract::<String>()?),
                Some(portal_video::limits_from_writer(
                    writer,
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
            dither,
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
            let bytes = read_source(&fs, &request.source, max_input_bytes)?;
            let decoded = self.decoder.decode(&bytes).map_err(|error| {
                let message = format!("sound cue {}: {error}", request.source.identity());
                decode_message(&error, message)
            })?;
            let input = PortalAudioInput {
                source: request.source.clone(),
                channels: decoded.audio.channels,
                sample_rate: decoded.audio.sample_rate,
                sample_frames: (decoded.audio.samples.len() / usize::from(decoded.audio.channels))
                    as u64,
                report: Arc::new(decoded.report),
            };
            let (frame, fps, time_offset) = match request.absolute_time {
                Some(time) => {
                    let sample =
                        absolute_sample_position(time, config.sample_rate).map_err(native_error)?;
                    let sample = if window.is_some() {
                        sample
                    } else {
                        timeline.output_sample(sample, request.time.fps(), config.sample_rate)?
                    };
                    (sample, config.sample_rate, 0.0)
                }
                None => (
                    if window.is_some() {
                        request.time.frames()
                    } else {
                        timeline.output_frame(request.time.frames())?
                    },
                    request.time.fps(),
                    request.time_offset,
                ),
            };
            mixer
                .add(fmn_output::SoundCue {
                    audio: decoded.audio,
                    frame,
                    fps,
                    time_offset,
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

    pub(crate) const fn dither_seed(&self) -> Option<u64> {
        match self.dither {
            DitherPolicy::None => None,
            DitherPolicy::Tpdf { seed } => Some(seed),
        }
    }
}

fn dither_from_writer(
    writer: &Bound<'_, PyAny>,
    format: &PortalOutputFormat,
) -> PyResult<DitherPolicy> {
    let value = match writer.getattr("audio_dither_seed") {
        Ok(value) if value.is_none() => return Ok(DitherPolicy::None),
        Ok(value) => value,
        Err(error) if error.is_instance_of::<PyAttributeError>(writer.py()) => {
            return Ok(DitherPolicy::None);
        }
        Err(error) => return Err(error),
    };
    if !matches!(format, PortalOutputFormat::Wav) {
        return Err(PyValueError::new_err(
            "file_writer.audio_dither_seed requires native wav output",
        ));
    }
    if value.is_instance_of::<PyBool>() {
        return Err(PyTypeError::new_err(
            "file_writer.audio_dither_seed must be an unsigned 64-bit integer, not bool",
        ));
    }
    Ok(DitherPolicy::Tpdf {
        seed: value.extract::<u64>()?,
    })
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
        match &input.source {
            SoundSource::File(path) => {
                // Preserve the existing file receipt schema, including all
                // checkpoint consumers. Memory sources have no filesystem path.
                fact.set_item("path", path.to_string_lossy().as_ref())?;
            }
            SoundSource::Memory(_) => {
                fact.set_item("path", scene.py().None())?;
                fact.set_item("source_kind", input.source.kind())?;
                fact.set_item("source", input.source.identity().as_ref())?;
            }
        }
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

/// Read a file through the admitted capability, or borrow frozen native WAV.
/// The cap applies before decoding in either case. File data keeps its owned
/// read buffer, while memory data needs neither a copy nor a filesystem call.
fn read_source<'a>(
    fs: &dyn fmn_platform::fs::FileSystem,
    source: &'a SoundSource,
    limit: usize,
) -> PyResult<Cow<'a, [u8]>> {
    match source {
        SoundSource::File(path) => fs
            .read_bounded(path, limit)
            .map(Cow::Owned)
            .map_err(|error| PyOSError::new_err(format!("sound cue {}: {error}", path.display()))),
        SoundSource::Memory(data) if data.bytes().len() <= limit => Ok(Cow::Borrowed(data.bytes())),
        SoundSource::Memory(_) => Err(PyValueError::new_err(format!(
            "sound cue {} exceeds the {limit}-byte source budget",
            source.identity()
        ))),
    }
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
        use fmn_platform::fs::StdFs;
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
        let minimum = frames_to_samples(i64::from(picture_frames), time.fps(), options.sample_rate)
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
            let (audio, report) = match cache.entry(request.source.key()) {
                Entry::Occupied(entry) => entry.into_mut(),
                Entry::Vacant(entry) => {
                    let raw_budget = (options.max_bytes - source_bytes)
                        .min(64 * 1024 * 1024)
                        .min(
                            self.ffmpeg_limits
                                .as_ref()
                                .map_or(usize::MAX, |policy| policy.max_input_bytes as usize),
                        );
                    let sample_budget = (options.max_bytes - decoded_bytes) / 4;
                    if raw_budget == 0 || sample_budget == 0 {
                        return Err(PyRuntimeError::new_err(
                            "bundle soundtrack source/decode budget exhausted",
                        ));
                    }
                    let bytes = read_source(&StdFs, &request.source, raw_budget)?;
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
                                format!("sound cue {}: {e}", request.source.identity()),
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
                    source: request.source.clone(),
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
            let (frame, fps, time_offset) = match request.absolute_time {
                Some(time) => (
                    absolute_sample_position(time, options.sample_rate).map_err(native_error)?,
                    options.sample_rate,
                    0.0,
                ),
                None => (
                    request.time.frames(),
                    request.time.fps(),
                    request.time_offset,
                ),
            };
            mixer
                .add(SoundCue {
                    audio: audio.clone(),
                    frame,
                    fps,
                    time_offset,
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

/// Exercise immutable Python PCM through native writer timing, WAV publication
/// and overlapping live inserts at three worker counts. Returns verified files.
///
/// # Errors
/// A Python assertion, native audio refusal or publication failure.
#[cfg(any(test, feature = "gauntlet"))]
pub fn run_portal_gauntlet_sound_data() -> Result<u64, String> {
    crate::with_python_test_module("immutable Python sound data", |py, _module, globals| {
        let source = std::ffi::CString::new(r#"
from pathlib import Path
import tempfile
import wave
import numpy as np
import manimlib as m
from fmn_python import SoundData, render_session, record_scene

def read_pcm(path):
    with wave.open(str(path), 'rb') as stream:
        assert (stream.getframerate(), stream.getnchannels(), stream.getsampwidth()) == (48000, 2, 2)
        return np.frombuffer(stream.readframes(stream.getnframes()), dtype='<i2').reshape(-1, 2)

original = np.array([.25, -.5] * 32, dtype=np.float32)
sound = SoundData(original, channels=2)
original[:] = 0
expected = np.tile(np.array([[8192, -16384]], dtype=np.int16), (32, 1))
verified_files = 0
digests = []
with tempfile.TemporaryDirectory(prefix='fmn sound data e2e ') as directory:
    root = Path(directory)
    for threads in (1, 4, 16):
        scene = m.Scene(file_writer_config={'ffmpeg_bin': str(root / 'no-decoder')})
        with render_session(scene, root / f'whole-{threads}.wav', fps=29, threads=threads) as whole:
            scene.wait_frames(1)
            scene.file_writer.add_audio_segment(sound, time=9 / 48000)
            scene.wait_frames(1)
        pcm = read_pcm(whole.result.destination)
        assert len(pcm) == 3310 and not pcm[:9].any() and not pcm[41:].any()
        np.testing.assert_array_equal(pcm[9:41], expected)
        assert whole.result.ffmpeg_invocations == ()
        assert whole.result.audio_inputs[0]['source_sha256'] == sound.sha256
        verified_files += 1
        with record_scene(scene, root / f'insert-{threads}.wav', threads=threads) as insert:
            scene.file_writer.add_sound(sound, time=2 / 29 - 2 / 48000)
            scene.wait_frames(1)
        pcm = read_pcm(insert.result.destination)
        np.testing.assert_array_equal(pcm[:30], expected[2:])
        assert not pcm[30:].any() and len(insert.result.audio_inputs) == 1
        assert insert.result.ffmpeg_invocations == ()
        verified_files += 1
        digests.append((whole.result.digest, insert.result.digest))
assert len(set(digests)) == 1
"#).map_err(|error| error.to_string())?;
        py.run(source.as_c_str(), Some(globals), Some(globals))
            .map_err(|error| error.to_string())?;
        globals
            .get_item("verified_files")
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "sound data scenario omitted its verification count".to_owned())?
            .extract::<u64>()
            .map_err(|error| error.to_string())
    })
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

    #[test]
    fn immutable_sound_data_wav_and_live_inserts() {
        assert_eq!(super::run_portal_gauntlet_sound_data().unwrap(), 6);
    }
}
