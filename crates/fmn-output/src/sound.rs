//! Deterministic WAV/PCM mixing (§14.5, fm-0m7, BN-14).
//!
//! The mixer consumes [`WavAudio`] buffers in the native decoder's normalized
//! representation. Mono is duplicated to stereo; stereo is averaged to mono;
//! no other channel layout is guessed. Sample-rate conversion uses
//! [`RESAMPLER_NAME`]: a 64-tap Blackman-windowed sinc evaluated from an exact
//! output-index/input-rate ratio, never a cumulative phase accumulator. Its
//! public quality contract is ≤0.1 dB passband loss through 80% of the lower
//! Nyquist frequency and at least 60 dB rejection from 150% of that Nyquist
//! frequency onward. The acceptance tests measure both bounds.
//!
//! Cues are mixed in `add` order. For every output sample, each cue first
//! applies `gain_to_background` to the already-mixed value during its active
//! interval, then adds its own sample after `gain`; both values are dB, matching
//! the Reference. Parallel work splits only the output timeline, so every
//! sample observes that same cue order at every thread count.
//!
//! Placement converts `(frame, fps)` to the nearest sample by exact integer
//! arithmetic, with ties away from zero. A user `time_offset` is converted from
//! the exact value of its input `f64` by the same rule. Negative starts clip
//! against sample zero; positive ends extend the mix. Final samples clamp to
//! `[-1, 1]`, and [`MixReport::clipped_samples`] is the warning counter.

use std::fmt;

use fmn_codec::{SampleFormat, WavAudio, encode_wav};

/// The stable name recorded in documentation and provenance for sample-rate
/// conversion.
pub const RESAMPLER_NAME: &str = "blackman-windowed-sinc-64";

const RESAMPLER_RADIUS: i64 = 32;
const DEFAULT_SAMPLE_RATE: u32 = 48_000;
const DEFAULT_CHANNELS: u16 = 2;
const DEFAULT_MAX_OUTPUT_FRAMES: u64 = 48_000_u64 * 60 * 60;
const MAX_MIX_THREADS: usize = 1_024;
const DB_DIVISOR: f64 = 20.0;
const S16_STEP: f64 = 1.0 / 32_768.0;

/// Typed sound-mixer refusals.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SoundError {
    /// A mixer or cue parameter is structurally invalid.
    InvalidConfig(&'static str),
    /// Only the explicitly defined mono/stereo matrix is accepted.
    UnsupportedChannels {
        /// Refused channel count.
        channels: u16,
    },
    /// Interleaved sample storage does not contain whole frames.
    MisalignedSamples {
        /// Channel count used for the alignment check.
        channels: u16,
        /// Total interleaved samples.
        samples: usize,
    },
    /// An input sample is NaN or infinite.
    NonFiniteSample {
        /// Interleaved sample index.
        index: usize,
    },
    /// A dB parameter is NaN, infinite, or converts outside finite amplitude.
    InvalidGain {
        /// Stable parameter name.
        parameter: &'static str,
    },
    /// Exact frame/offset conversion cannot fit the signed sample timeline.
    PlacementOutOfRange,
    /// Resampling or channel conversion would exceed addressable storage.
    SampleCountOverflow,
    /// A cue would extend the positive timeline beyond the configured budget.
    OutputTooLong {
        /// Required output frames.
        frames: u64,
        /// Configured limit.
        max_frames: u64,
    },
    /// Thread count zero has no execution meaning.
    ZeroThreads,
    /// A hostile thread count is refused before spawning.
    TooManyThreads {
        /// Requested workers.
        requested: usize,
        /// Hard process-local limit.
        max: usize,
    },
    /// A scoped worker panicked while writing its disjoint sample range.
    WorkerPanicked,
    /// Native certified output is s16 or f32 WAV.
    UnsupportedOutputFormat {
        /// Refused format.
        format: SampleFormat,
    },
    /// Dither only has meaning when reducing to integer PCM.
    DitherOnFloatOutput,
}

impl fmt::Display for SoundError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidConfig(message) => write!(f, "invalid sound configuration: {message}"),
            Self::UnsupportedChannels { channels } => {
                write!(
                    f,
                    "unsupported {channels}-channel audio; only mono/stereo are defined"
                )
            }
            Self::MisalignedSamples { channels, samples } => write!(
                f,
                "{samples} interleaved samples do not form whole {channels}-channel frames"
            ),
            Self::NonFiniteSample { index } => {
                write!(f, "audio sample {index} is NaN or infinite")
            }
            Self::InvalidGain { parameter } => {
                write!(f, "{parameter} must be finite and yield finite amplitude")
            }
            Self::PlacementOutOfRange => {
                f.write_str("sound placement is outside the signed sample timeline")
            }
            Self::SampleCountOverflow => {
                f.write_str("sound sample count exceeds addressable storage")
            }
            Self::OutputTooLong { frames, max_frames } => write!(
                f,
                "sound mix needs {frames} frames, exceeding the {max_frames}-frame budget"
            ),
            Self::ZeroThreads => f.write_str("sound mixer thread count must be nonzero"),
            Self::TooManyThreads { requested, max } => write!(
                f,
                "sound mixer requested {requested} threads, exceeding the {max}-thread limit"
            ),
            Self::WorkerPanicked => f.write_str("sound mixer worker panicked"),
            Self::UnsupportedOutputFormat { format } => {
                write!(f, "sound WAV output does not support {format:?}")
            }
            Self::DitherOnFloatOutput => f.write_str("dither is only valid for integer PCM output"),
        }
    }
}

impl std::error::Error for SoundError {}

/// Output matrix and resource budget.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MixerConfig {
    /// Output sample rate.
    pub sample_rate: u32,
    /// Output channel count (one or two).
    pub channels: u16,
    /// Maximum frames in any prepared cue and in the positive output timeline.
    pub max_output_frames: u64,
}

impl Default for MixerConfig {
    fn default() -> Self {
        Self {
            sample_rate: DEFAULT_SAMPLE_RATE,
            channels: DEFAULT_CHANNELS,
            max_output_frames: DEFAULT_MAX_OUTPUT_FRAMES,
        }
    }
}

impl MixerConfig {
    fn validate(self) -> Result<Self, SoundError> {
        if self.sample_rate == 0 {
            return Err(SoundError::InvalidConfig("sample_rate must be nonzero"));
        }
        if !matches!(self.channels, 1 | 2) {
            return Err(SoundError::UnsupportedChannels {
                channels: self.channels,
            });
        }
        if self.max_output_frames == 0 {
            return Err(SoundError::InvalidConfig(
                "max_output_frames must be nonzero",
            ));
        }
        let addressable = u64::try_from(usize::MAX).unwrap_or(u64::MAX);
        let max_by_samples = addressable / u64::from(self.channels);
        if self.max_output_frames > max_by_samples || self.max_output_frames > i64::MAX as u64 {
            return Err(SoundError::InvalidConfig(
                "max_output_frames exceeds the signed addressable timeline",
            ));
        }
        Ok(self)
    }
}

/// One ordered source to place on the output timeline.
#[derive(Debug, Clone, PartialEq)]
pub struct SoundCue {
    /// Decoded native PCM.
    pub audio: WavAudio,
    /// Exact Scene frame at which `add_sound` was called.
    pub frame: i64,
    /// Scene frame-grid denominator.
    pub fps: u32,
    /// Reference-compatible relative offset in seconds.
    pub time_offset: f64,
    /// Foreground gain in dB.
    pub gain: Option<f64>,
    /// Background gain in dB during this cue's active interval.
    pub gain_to_background: Option<f64>,
}

impl SoundCue {
    /// A cue at an exact frame-grid time with zero offset and unity gain.
    #[must_use]
    pub fn new(audio: WavAudio, frame: i64, fps: u32) -> Self {
        Self {
            audio,
            frame,
            fps,
            time_offset: 0.0,
            gain: None,
            gain_to_background: None,
        }
    }
}

#[derive(Debug)]
struct PreparedCue {
    start: i64,
    frames: usize,
    samples: Vec<f64>,
    gain: f64,
    background_gain: Option<f64>,
}

/// Dither policy for bit-depth reduction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DitherPolicy {
    /// Certified default: no dither.
    None,
    /// Deterministic TPDF with two counter-derived uniforms per sample.
    Tpdf {
        /// Content/provenance seed. Sample index, not worker schedule, advances
        /// the sequence.
        seed: u64,
    },
}

/// Mixed normalized PCM plus explicit warning/provenance counters.
#[derive(Debug, Clone, PartialEq)]
pub struct MixReport {
    /// Interleaved f32 output after the defined clamp.
    pub audio: WavAudio,
    /// Number of samples that exceeded `[-1, 1]` before clamping.
    pub clipped_samples: u64,
    /// Number of cues consumed in their insertion order.
    pub cues_mixed: usize,
    /// Actual disjoint timeline workers used (zero for an empty mix).
    pub workers_used: usize,
}

impl MixReport {
    /// Encode this report as native WAV with the declared reduction policy.
    ///
    /// # Errors
    /// [`SoundError::UnsupportedOutputFormat`] for formats other than s16/f32,
    /// or [`SoundError::DitherOnFloatOutput`] when dither is requested for f32.
    pub fn wav_bytes(
        &self,
        format: SampleFormat,
        dither: DitherPolicy,
    ) -> Result<Vec<u8>, SoundError> {
        validate_audio(&self.audio)?;
        match (format, dither) {
            (SampleFormat::F32, DitherPolicy::None) => Ok(encode_wav(
                self.audio.channels,
                self.audio.sample_rate,
                format,
                &self.audio.samples,
            )),
            (SampleFormat::F32, DitherPolicy::Tpdf { .. }) => Err(SoundError::DitherOnFloatOutput),
            (SampleFormat::S16, DitherPolicy::None) => Ok(encode_wav(
                self.audio.channels,
                self.audio.sample_rate,
                format,
                &self.audio.samples,
            )),
            (SampleFormat::S16, DitherPolicy::Tpdf { seed }) => {
                let mut reduced = self.audio.samples.clone();
                for (index, sample) in reduced.iter_mut().enumerate() {
                    let counter =
                        u64::try_from(index).map_err(|_| SoundError::SampleCountOverflow)?;
                    let noise = tpdf(seed, counter) * S16_STEP;
                    *sample = (f64::from(*sample) + noise).clamp(-1.0, 1.0) as f32;
                }
                Ok(encode_wav(
                    self.audio.channels,
                    self.audio.sample_rate,
                    format,
                    &reduced,
                ))
            }
            (format, _) => Err(SoundError::UnsupportedOutputFormat { format }),
        }
    }
}

/// An insertion-ordered deterministic sound timeline.
#[derive(Debug)]
pub struct SoundMixer {
    config: MixerConfig,
    cues: Vec<PreparedCue>,
}

impl SoundMixer {
    /// Construct an empty timeline.
    ///
    /// # Errors
    /// Invalid sample rate, channel matrix, or allocation budget.
    pub fn new(config: MixerConfig) -> Result<Self, SoundError> {
        Ok(Self {
            config: config.validate()?,
            cues: Vec::new(),
        })
    }

    /// Resolved output configuration.
    #[must_use]
    pub const fn config(&self) -> MixerConfig {
        self.config
    }

    /// Number of insertion-ordered cues.
    #[must_use]
    pub fn len(&self) -> usize {
        self.cues.len()
    }

    /// Whether no cues have been added.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.cues.is_empty()
    }

    /// Validate, map, resample, and append one cue.
    ///
    /// Resampling occurs once, before any timeline workers are selected; the
    /// worker count therefore cannot influence coefficients or source phase.
    ///
    /// # Errors
    /// Every malformed-input, placement, and resource refusal in
    /// [`SoundError`].
    pub fn add(&mut self, cue: SoundCue) -> Result<&mut Self, SoundError> {
        validate_audio(&cue.audio)?;
        if cue.fps == 0 {
            return Err(SoundError::InvalidConfig("cue fps must be nonzero"));
        }
        let gain = db_amplitude(cue.gain, "gain")?;
        let background_gain = db_amplitude_optional(cue.gain_to_background, "gain_to_background")?;
        let frame_start = round_ratio(
            i128::from(cue.frame) * i128::from(self.config.sample_rate),
            u128::from(cue.fps),
        )?;
        let offset = seconds_to_samples(cue.time_offset, self.config.sample_rate)?;
        let start = frame_start
            .checked_add(offset)
            .ok_or(SoundError::PlacementOutOfRange)?;

        let input_frames = cue.audio.samples.len() / usize::from(cue.audio.channels);
        let frames =
            resampled_frame_count(input_frames, cue.audio.sample_rate, self.config.sample_rate)?;
        let frame_count = u64::try_from(frames).map_err(|_| SoundError::SampleCountOverflow)?;
        let end = i128::from(start)
            + i128::try_from(frames).map_err(|_| SoundError::SampleCountOverflow)?;
        if frame_count > self.config.max_output_frames {
            return Err(SoundError::OutputTooLong {
                frames: frame_count,
                max_frames: self.config.max_output_frames,
            });
        }
        if frames > 0 && end > i128::from(self.config.max_output_frames) {
            let required = u64::try_from(end).map_err(|_| SoundError::SampleCountOverflow)?;
            return Err(SoundError::OutputTooLong {
                frames: required,
                max_frames: self.config.max_output_frames,
            });
        }
        let mapped = map_channels(&cue.audio, self.config.channels)?;
        let samples = resample(
            &mapped,
            self.config.channels,
            cue.audio.sample_rate,
            self.config.sample_rate,
        )?;

        self.cues.push(PreparedCue {
            start,
            frames,
            samples,
            gain,
            background_gain,
        });
        Ok(self)
    }

    /// Mix the complete timeline with disjoint output-range workers.
    ///
    /// # Errors
    /// An invalid worker count or a worker panic.
    pub fn mix(&self, threads: usize) -> Result<MixReport, SoundError> {
        if threads == 0 {
            return Err(SoundError::ZeroThreads);
        }
        if threads > MAX_MIX_THREADS {
            return Err(SoundError::TooManyThreads {
                requested: threads,
                max: MAX_MIX_THREADS,
            });
        }
        let total_frames = self
            .cues
            .iter()
            .filter_map(|cue| {
                let end = i128::from(cue.start) + cue.frames as i128;
                (cue.frames > 0 && end > 0).then_some(end)
            })
            .max()
            .unwrap_or(0);
        let total_frames =
            usize::try_from(total_frames).map_err(|_| SoundError::SampleCountOverflow)?;
        let channels = usize::from(self.config.channels);
        let sample_count = total_frames
            .checked_mul(channels)
            .ok_or(SoundError::SampleCountOverflow)?;
        let mut mixed = vec![0.0_f64; sample_count];

        let worker_target = threads.min(total_frames);
        let workers_used = if worker_target == 1 {
            mix_chunk(&mut mixed, 0, channels, &self.cues);
            1
        } else if worker_target > 1 {
            let chunk_frames = total_frames.div_ceil(worker_target);
            let chunk_samples = chunk_frames
                .checked_mul(channels)
                .ok_or(SoundError::SampleCountOverflow)?;
            let actual_workers = total_frames.div_ceil(chunk_frames);
            let worker_result = std::thread::scope(|scope| {
                let mut handles = Vec::with_capacity(actual_workers);
                for (chunk_index, chunk) in mixed.chunks_mut(chunk_samples).enumerate() {
                    let start_frame = chunk_index * chunk_frames;
                    let cues = &self.cues;
                    handles.push(scope.spawn(move || {
                        mix_chunk(chunk, start_frame, channels, cues);
                    }));
                }
                let mut panicked = false;
                for handle in handles {
                    panicked |= handle.join().is_err();
                }
                panicked
            });
            if worker_result {
                return Err(SoundError::WorkerPanicked);
            }
            actual_workers
        } else {
            0
        };

        let mut clipped_samples = 0_u64;
        let samples = mixed
            .into_iter()
            .map(|sample| {
                if !(-1.0..=1.0).contains(&sample) {
                    clipped_samples += 1;
                }
                sample.clamp(-1.0, 1.0) as f32
            })
            .collect();
        Ok(MixReport {
            audio: WavAudio {
                channels: self.config.channels,
                sample_rate: self.config.sample_rate,
                format: SampleFormat::F32,
                samples,
            },
            clipped_samples,
            cues_mixed: self.cues.len(),
            workers_used,
        })
    }
}

fn validate_audio(audio: &WavAudio) -> Result<(), SoundError> {
    if audio.sample_rate == 0 {
        return Err(SoundError::InvalidConfig(
            "source sample_rate must be nonzero",
        ));
    }
    if !matches!(audio.channels, 1 | 2) {
        return Err(SoundError::UnsupportedChannels {
            channels: audio.channels,
        });
    }
    let channels = usize::from(audio.channels);
    if !audio.samples.len().is_multiple_of(channels) {
        return Err(SoundError::MisalignedSamples {
            channels: audio.channels,
            samples: audio.samples.len(),
        });
    }
    if let Some(index) = audio.samples.iter().position(|sample| !sample.is_finite()) {
        return Err(SoundError::NonFiniteSample { index });
    }
    Ok(())
}

fn db_amplitude(value: Option<f64>, parameter: &'static str) -> Result<f64, SoundError> {
    Ok(db_amplitude_optional(value, parameter)?.unwrap_or(1.0))
}

fn db_amplitude_optional(
    value: Option<f64>,
    parameter: &'static str,
) -> Result<Option<f64>, SoundError> {
    let Some(db) = value else {
        return Ok(None);
    };
    if !db.is_finite() {
        return Err(SoundError::InvalidGain { parameter });
    }
    let amplitude = fmn_dmath::pow(10.0, db / DB_DIVISOR);
    if !amplitude.is_finite() {
        return Err(SoundError::InvalidGain { parameter });
    }
    Ok(Some(amplitude))
}

fn map_channels(audio: &WavAudio, output_channels: u16) -> Result<Vec<f64>, SoundError> {
    let input_channels = usize::from(audio.channels);
    let output_channels = usize::from(output_channels);
    let frames = audio.samples.len() / input_channels;
    let capacity = frames
        .checked_mul(output_channels)
        .ok_or(SoundError::SampleCountOverflow)?;
    let mut mapped = Vec::with_capacity(capacity);
    match (input_channels, output_channels) {
        (1, 1) | (2, 2) => {
            mapped.extend(audio.samples.iter().map(|&sample| f64::from(sample)));
        }
        (1, 2) => {
            for &sample in &audio.samples {
                let sample = f64::from(sample);
                mapped.extend_from_slice(&[sample, sample]);
            }
        }
        (2, 1) => {
            for frame in audio.samples.as_chunks::<2>().0 {
                mapped.push((f64::from(frame[0]) + f64::from(frame[1])) * 0.5);
            }
        }
        _ => {
            return Err(SoundError::UnsupportedChannels {
                channels: audio.channels,
            });
        }
    }
    Ok(mapped)
}

fn resample(
    input: &[f64],
    channels: u16,
    input_rate: u32,
    output_rate: u32,
) -> Result<Vec<f64>, SoundError> {
    if input_rate == output_rate || input.is_empty() {
        return Ok(input.to_vec());
    }
    let channels = usize::from(channels);
    let input_frames = input.len() / channels;
    let output_frames = resampled_frame_count(input_frames, input_rate, output_rate)?;
    let output_samples = output_frames
        .checked_mul(channels)
        .ok_or(SoundError::SampleCountOverflow)?;
    let input_frames_i64 =
        i64::try_from(input_frames).map_err(|_| SoundError::SampleCountOverflow)?;
    let cutoff = (f64::from(output_rate) / f64::from(input_rate)).min(1.0);
    let mut output = vec![0.0_f64; output_samples];
    for output_frame in 0..output_frames {
        let source_numerator = (output_frame as u128) * u128::from(input_rate);
        let base = source_numerator / u128::from(output_rate);
        let remainder = source_numerator % u128::from(output_rate);
        let base = i64::try_from(base).map_err(|_| SoundError::SampleCountOverflow)?;
        let fraction = remainder as f64 / f64::from(output_rate);
        let mut sums = [0.0_f64; 2];
        let mut weight_sum = 0.0_f64;
        for delta in (-RESAMPLER_RADIUS + 1)..=RESAMPLER_RADIUS {
            let source_frame = base + delta;
            if !(0..input_frames_i64).contains(&source_frame) {
                continue;
            }
            let distance = delta as f64 - fraction;
            let window_phase = std::f64::consts::PI * distance / RESAMPLER_RADIUS as f64;
            let window = 0.42
                + 0.5 * fmn_dmath::cos(window_phase)
                + 0.08 * fmn_dmath::cos(2.0 * window_phase);
            let sinc_x = cutoff * distance;
            let sinc = if sinc_x == 0.0 {
                1.0
            } else {
                let phase = std::f64::consts::PI * sinc_x;
                fmn_dmath::sin(phase) / phase
            };
            let weight = cutoff * sinc * window;
            let source_frame =
                usize::try_from(source_frame).map_err(|_| SoundError::SampleCountOverflow)?;
            let source_at = source_frame * channels;
            for channel in 0..channels {
                sums[channel] += input[source_at + channel] * weight;
            }
            weight_sum += weight;
        }
        let output_at = output_frame * channels;
        for channel in 0..channels {
            output[output_at + channel] = sums[channel] / weight_sum;
        }
    }
    Ok(output)
}

fn resampled_frame_count(
    input_frames: usize,
    input_rate: u32,
    output_rate: u32,
) -> Result<usize, SoundError> {
    let numerator = (input_frames as u128) * u128::from(output_rate);
    let output_frames = numerator.div_ceil(u128::from(input_rate));
    usize::try_from(output_frames).map_err(|_| SoundError::SampleCountOverflow)
}

fn mix_chunk(output: &mut [f64], start_frame: usize, channels: usize, cues: &[PreparedCue]) {
    for (relative_frame, frame) in output.chunks_exact_mut(channels).enumerate() {
        let global_frame = start_frame + relative_frame;
        for (channel, destination) in frame.iter_mut().enumerate() {
            let mut sample = 0.0_f64;
            for cue in cues {
                let local = global_frame as i128 - i128::from(cue.start);
                if local < 0 || local >= cue.frames as i128 {
                    continue;
                }
                if let Some(background_gain) = cue.background_gain {
                    sample *= background_gain;
                }
                let local = local as usize;
                sample += cue.samples[local * channels + channel] * cue.gain;
            }
            *destination = sample;
        }
    }
}

fn round_ratio(numerator: i128, denominator: u128) -> Result<i64, SoundError> {
    let negative = numerator.is_negative();
    let magnitude = numerator.unsigned_abs();
    let whole = magnitude / denominator;
    let remainder = magnitude % denominator;
    let rounded = whole + u128::from(remainder >= denominator.div_ceil(2));
    signed_i64(rounded, negative)
}

fn seconds_to_samples(seconds: f64, sample_rate: u32) -> Result<i64, SoundError> {
    if !seconds.is_finite() {
        return Err(SoundError::PlacementOutOfRange);
    }
    let bits = seconds.to_bits();
    let negative = bits >> 63 != 0;
    let biased = ((bits >> 52) & 0x7ff) as i32;
    let fraction = bits & 0x000f_ffff_ffff_ffff;
    let (mantissa, exponent) = if biased == 0 {
        (fraction, -1074)
    } else {
        (fraction | (1_u64 << 52), biased - 1075)
    };
    if mantissa == 0 {
        return Ok(0);
    }
    let magnitude = u128::from(mantissa) * u128::from(sample_rate);
    let rounded = if exponent >= 0 {
        magnitude
            .checked_shl(exponent as u32)
            .ok_or(SoundError::PlacementOutOfRange)?
    } else {
        let shift = exponent.unsigned_abs();
        if shift >= 128 {
            0
        } else {
            let whole = magnitude >> shift;
            let remainder_mask = (1_u128 << shift) - 1;
            let remainder = magnitude & remainder_mask;
            whole + u128::from(remainder >= (1_u128 << (shift - 1)))
        }
    };
    signed_i64(rounded, negative)
}

fn signed_i64(magnitude: u128, negative: bool) -> Result<i64, SoundError> {
    if negative {
        let limit = (i64::MAX as u128) + 1;
        if magnitude > limit {
            return Err(SoundError::PlacementOutOfRange);
        }
        if magnitude == limit {
            Ok(i64::MIN)
        } else {
            let value = i64::try_from(magnitude).map_err(|_| SoundError::PlacementOutOfRange)?;
            Ok(-value)
        }
    } else {
        i64::try_from(magnitude).map_err(|_| SoundError::PlacementOutOfRange)
    }
}

fn splitmix64(mut value: u64) -> u64 {
    value = value.wrapping_add(0x9e37_79b9_7f4a_7c15);
    value = (value ^ (value >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    value = (value ^ (value >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    value ^ (value >> 31)
}

fn unit_f64(value: u64) -> f64 {
    (value >> 11) as f64 * (1.0 / 9_007_199_254_740_992.0)
}

fn tpdf(seed: u64, counter: u64) -> f64 {
    let first = splitmix64(seed ^ counter.wrapping_mul(0xd2b7_4407_b1ce_6e93));
    let second = splitmix64(seed ^ counter.wrapping_mul(0xca5a_8263_9512_1157) ^ u64::MAX);
    unit_f64(first) - unit_f64(second)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn audio(sample_rate: u32, channels: u16, format: SampleFormat, samples: &[f32]) -> WavAudio {
        WavAudio {
            channels,
            sample_rate,
            format,
            samples: samples.to_vec(),
        }
    }

    fn config(sample_rate: u32, channels: u16) -> MixerConfig {
        MixerConfig {
            sample_rate,
            channels,
            max_output_frames: 100_000,
        }
    }

    fn cue(samples: &[f32], frame: i64, fps: u32) -> SoundCue {
        SoundCue::new(audio(8, 1, SampleFormat::F32, samples), frame, fps)
    }

    #[test]
    fn golden_offsets_overlaps_negative_clip_and_past_end() {
        let mut mixer = SoundMixer::new(config(8, 1)).expect("mixer");
        mixer
            .add(cue(&[0.25, 0.25, 0.25, 0.25], 0, 4))
            .expect("background");
        mixer
            .add(cue(&[0.5, 0.5, 0.5], 1, 4))
            .expect("past-end overlay");
        let mut negative = cue(&[1.0, 0.75, 0.5, 0.25], 0, 4);
        negative.time_offset = -0.25;
        mixer.add(negative).expect("pre-zero overlay");

        let report = mixer.mix(4).expect("mix");
        assert_eq!(report.audio.samples, [0.75, 0.5, 0.75, 0.75, 0.5]);
        assert_eq!(report.clipped_samples, 0);
        let wav = report
            .wav_bytes(SampleFormat::S16, DitherPolicy::None)
            .expect("certified wav");
        assert_eq!(
            fmn_hash::sha256::sha256(&wav).to_hex(),
            "c8aeb8739332df3a03f4ea350a2ba0d0a0ab0fedd1638191763b227dcd975710"
        );
    }

    #[test]
    fn gain_and_background_duck_are_db_and_insertion_ordered() {
        let half_db = -6.020_599_913_279_624;
        let mut mixer = SoundMixer::new(config(8, 1)).expect("mixer");
        mixer.add(cue(&[0.8, 0.8], 0, 8)).expect("background");
        let mut foreground = cue(&[0.4, 0.4], 0, 8);
        foreground.gain = Some(half_db);
        foreground.gain_to_background = Some(half_db);
        mixer.add(foreground).expect("foreground");
        let report = mixer.mix(1).expect("mix");
        for sample in report.audio.samples {
            assert!((sample - 0.6).abs() < 1.0e-6, "{sample}");
        }

        let mut reversed = SoundMixer::new(config(8, 1)).expect("mixer");
        let mut foreground = cue(&[0.4], 0, 8);
        foreground.gain_to_background = Some(half_db);
        reversed.add(foreground).expect("foreground");
        reversed.add(cue(&[0.8], 0, 8)).expect("background");
        assert_eq!(
            reversed.mix(1).expect("mix").audio.samples,
            [1.0],
            "ducking affects only the already-mixed background"
        );
    }

    #[test]
    fn i16_i24_f32_share_the_defined_channel_matrix() {
        for format in [SampleFormat::S16, SampleFormat::S24, SampleFormat::F32] {
            let mut stereo = SoundMixer::new(config(8, 2)).expect("stereo");
            stereo
                .add(SoundCue::new(audio(8, 1, format, &[0.25, -0.5]), 0, 8))
                .expect("mono source");
            assert_eq!(
                stereo.mix(2).expect("mix").audio.samples,
                [0.25, 0.25, -0.5, -0.5]
            );

            let mut mono = SoundMixer::new(config(8, 1)).expect("mono");
            mono.add(SoundCue::new(
                audio(8, 2, format, &[1.0, -1.0, 0.5, 0.25]),
                0,
                8,
            ))
            .expect("stereo source");
            assert_eq!(mono.mix(2).expect("mix").audio.samples, [0.0, 0.375]);
        }
    }

    fn tone(rate: u32, frequency: u32, frames: usize) -> Vec<f32> {
        (0..frames)
            .map(|index| {
                let phase = 2.0 * std::f64::consts::PI * f64::from(frequency) * index as f64
                    / f64::from(rate);
                (0.5 * fmn_dmath::sin(phase)) as f32
            })
            .collect()
    }

    fn rms(samples: &[f32]) -> f64 {
        let energy = samples
            .iter()
            .map(|&sample| f64::from(sample) * f64::from(sample))
            .sum::<f64>();
        (energy / samples.len() as f64).sqrt()
    }

    fn resampled_tone(frequency: u32) -> Vec<f32> {
        let mut mixer = SoundMixer::new(config(16_000, 1)).expect("mixer");
        mixer
            .add(SoundCue::new(
                audio(
                    48_000,
                    1,
                    SampleFormat::F32,
                    &tone(48_000, frequency, 12_288),
                ),
                0,
                16_000,
            ))
            .expect("resample");
        mixer.mix(4).expect("mix").audio.samples
    }

    #[test]
    fn blackman_sinc_64_meets_its_passband_and_stopband_contract() {
        let pass = resampled_tone(6_400);
        let pass = &pass[128..pass.len() - 128];
        let expected_rms = 0.5 / 2.0_f64.sqrt();
        let pass_ratio = rms(pass) / expected_rms;
        assert!(
            (0.988_553..=1.011_58).contains(&pass_ratio),
            "{RESAMPLER_NAME} passband ratio {pass_ratio}"
        );

        let stop = resampled_tone(12_000);
        let stop = &stop[128..stop.len() - 128];
        let stop_ratio = rms(stop) / expected_rms;
        assert!(
            stop_ratio <= 0.001,
            "{RESAMPLER_NAME} stopband ratio {stop_ratio} exceeds -60 dB"
        );
    }

    #[test]
    fn clipping_is_counted_and_tpdf_is_seeded_per_sample() {
        let mut clipping = SoundMixer::new(config(8, 1)).expect("mixer");
        clipping.add(cue(&[0.75, -0.75], 0, 8)).expect("first");
        clipping.add(cue(&[0.75, -0.75], 0, 8)).expect("second");
        let clipped = clipping.mix(2).expect("mix");
        assert_eq!(clipped.audio.samples, [1.0, -1.0]);
        assert_eq!(clipped.clipped_samples, 2);

        let mut quiet = SoundMixer::new(config(8, 1)).expect("mixer");
        quiet
            .add(cue(&[0.001, 0.002, 0.003, 0.004], 0, 8))
            .expect("quiet cue");
        let quiet = quiet.mix(1).expect("mix");
        let a = quiet
            .wav_bytes(SampleFormat::S16, DitherPolicy::Tpdf { seed: 17 })
            .expect("dither");
        let b = quiet
            .wav_bytes(SampleFormat::S16, DitherPolicy::Tpdf { seed: 17 })
            .expect("dither");
        let c = quiet
            .wav_bytes(SampleFormat::S16, DitherPolicy::Tpdf { seed: 18 })
            .expect("dither");
        assert_eq!(a, b);
        assert_ne!(a, c);
        assert!(matches!(
            quiet.wav_bytes(SampleFormat::F32, DitherPolicy::Tpdf { seed: 17 }),
            Err(SoundError::DitherOnFloatOutput)
        ));
    }

    #[test]
    fn wav_bytes_are_identical_at_one_four_and_sixteen_threads() {
        let mut mixer = SoundMixer::new(config(48_000, 2)).expect("mixer");
        let source = tone(44_100, 997, 2_048);
        for index in 0..7 {
            let mut placed =
                SoundCue::new(audio(44_100, 1, SampleFormat::S24, &source), index * 3, 30);
            placed.gain = Some(-f64::from(index as u32));
            placed.gain_to_background = (index % 2 == 0).then_some(-1.5);
            mixer.add(placed).expect("cue");
        }
        let one = mixer.mix(1).expect("one");
        let four = mixer.mix(4).expect("four");
        let sixteen = mixer.mix(16).expect("sixteen");
        assert_eq!(one.audio.samples, four.audio.samples);
        assert_eq!(one.audio.samples, sixteen.audio.samples);
        let encode = |report: &MixReport| {
            report
                .wav_bytes(SampleFormat::S16, DitherPolicy::Tpdf { seed: 99 })
                .expect("wav")
        };
        assert_eq!(encode(&one), encode(&four));
        assert_eq!(encode(&one), encode(&sixteen));
    }

    #[test]
    fn click_on_frame_n_lands_on_the_exact_sample() {
        let mut mixer = SoundMixer::new(config(48_000, 1)).expect("mixer");
        mixer
            .add(SoundCue::new(
                audio(48_000, 1, SampleFormat::S16, &[1.0]),
                37,
                30,
            ))
            .expect("click");
        let report = mixer.mix(16).expect("mix");
        let expected = 37 * (48_000 / 30);
        assert_eq!(report.audio.samples.len(), expected + 1);
        assert!(report.audio.samples[..expected].iter().all(|&x| x == 0.0));
        assert_eq!(report.audio.samples[expected], 1.0);
    }

    #[test]
    fn malformed_inputs_and_resource_overruns_are_named() {
        assert!(matches!(
            SoundMixer::new(config(8, 3)),
            Err(SoundError::UnsupportedChannels { channels: 3 })
        ));
        let mut mixer = SoundMixer::new(MixerConfig {
            sample_rate: 8,
            channels: 1,
            max_output_frames: 4,
        })
        .expect("mixer");
        assert!(matches!(
            mixer.add(SoundCue::new(
                audio(8, 1, SampleFormat::F32, &[0.0; 5]),
                0,
                8
            )),
            Err(SoundError::OutputTooLong {
                frames: 5,
                max_frames: 4
            })
        ));
        assert!(matches!(mixer.mix(0), Err(SoundError::ZeroThreads)));
        assert!(matches!(
            mixer.mix(MAX_MIX_THREADS + 1),
            Err(SoundError::TooManyThreads { .. })
        ));

        let mut empty = SoundMixer::new(MixerConfig {
            sample_rate: 8,
            channels: 1,
            max_output_frames: 4,
        })
        .expect("mixer");
        empty
            .add(SoundCue::new(audio(8, 1, SampleFormat::F32, &[]), 100, 1))
            .expect("empty audio does not extend the timeline");
        assert!(empty.mix(1).expect("empty mix").audio.samples.is_empty());

        let mut bounded = SoundMixer::new(MixerConfig {
            sample_rate: u32::MAX,
            channels: 1,
            max_output_frames: 4,
        })
        .expect("mixer");
        assert!(matches!(
            bounded.add(SoundCue::new(
                audio(1, 1, SampleFormat::F32, &[0.0]),
                0,
                1
            )),
            Err(SoundError::OutputTooLong {
                frames,
                max_frames: 4
            }) if frames == u64::from(u32::MAX)
        ));
    }
}
