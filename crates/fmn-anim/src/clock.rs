//! The RationalFrameClock (§9.2, D-07, BN-02): manim's exact nominal
//! sample points, drift-free.
//!
//! The Reference advances a float accumulator over
//! `np.arange(0, run_time, 1/fps) + 1/fps`; at 30 fps an hour of frames
//! drifts visibly and two runs disagree under different chunkings. Here
//! time never accumulates — it *derives*: the clock is `(frame_index,
//! fps)`, a [`RationalTime`] whose value is exactly `frames / fps`.
//!
//! The emission semantics kept from the Reference, exactly:
//! - samples are `t_k = k / fps` for `k = 1..=n` — **no alpha-zero frame**
//!   in the emission sequence (`begin()` interpolates at zero separately);
//! - **upward duration rounding**: `n = ceil(run_time · fps)` computed on
//!   the exact rational value of the `f64` duration, so a run_time that is
//!   not a whole number of frames still covers its end;
//! - the final sample may exceed `run_time`; **alpha clamps to [0, 1]**
//!   (the Reference's `interpolate` clip);
//! - skipped playback visits the same grid samples while suppressing capture,
//!   so state evolution is identical rather than merely time-equivalent.
//!
//! **PERMANENT REFUSAL (D-18, §10.5):** adaptive or variable frame
//! sampling is refused forever. The type system encodes it: a
//! [`RationalTime`] is only constructible as a whole number of frames
//! over the clock's fps — there is no API that emits an off-grid sample.
//!
//! Behavior Note **BN-02** documents the deliberate divergence: frame
//! counts can differ by one from the Python engine exactly where binary
//! `f64` representation makes the requested duration land off its decimal
//! intent (e.g. `0.1` at 30 fps is 4 frames here — the f64 is strictly
//! greater than 1/10, and three frames genuinely do not cover it — where
//! float `arange` happens to produce 3).

/// An authored duration. Seconds keep BN-02's deliberate ceiling; frames
/// bypass that conversion and are counted exactly on the receiving clock.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Duration {
    /// An arbitrary floating-point number of seconds.
    Seconds(f64),
    /// An exact number of frames at the scene's frame rate.
    Frames(u64),
}

impl From<f64> for Duration {
    fn from(seconds: f64) -> Self {
        Self::Seconds(seconds)
    }
}

/// A duration resolved onto a frame grid, before upward rounding.
///
/// Composition needs more than an integer: a half-frame lag followed by an
/// integer-frame animation must be added before the final ceiling. Both
/// `f64` seconds multiplied by integer fps and integer frames are dyadic
/// rationals. Keeping that exact value here avoids an intermediate float
/// round-trip, including for frame counts above `2^53`.
///
/// Arithmetic is bounded to 16,384 significant binary places. Excessively
/// deep, tiny-lag compositions return a named error instead of allocating
/// without a limit or silently rounding a duration.
#[derive(Debug, Clone, PartialEq)]
pub struct FrameDuration {
    value: Dyadic,
    seconds: Option<f64>,
    fps: u32,
}

const MAX_DURATION_WORDS: usize = 256;
const MAX_DURATION_BITS: i32 = (MAX_DURATION_WORDS * 64) as i32;

// Ordinary frame positions, mixed seconds and several nested retimings fit
// inline. Planning can still represent the full bounded precision budget.
const INLINE_DURATION_WORDS: usize = 8;

#[derive(Debug, Clone)]
enum Words {
    Inline {
        data: [u64; INLINE_DURATION_WORDS],
        len: usize,
    },
    Wide(Vec<u64>),
}

impl Words {
    fn with_capacity(capacity: usize) -> Self {
        if capacity <= INLINE_DURATION_WORDS {
            Self::Inline {
                data: [0; INLINE_DURATION_WORDS],
                len: 0,
            }
        } else {
            Self::Wide(Vec::with_capacity(capacity))
        }
    }

    fn from_slice(words: &[u64]) -> Self {
        let mut out = Self::with_capacity(words.len());
        for word in words {
            out.push(*word);
        }
        out
    }

    fn push(&mut self, word: u64) {
        match self {
            Self::Inline { data, len } if *len < INLINE_DURATION_WORDS => {
                data[*len] = word;
                *len += 1;
            }
            Self::Inline { data, len } => {
                let mut words = Vec::with_capacity(*len + 1);
                words.extend_from_slice(&data[..*len]);
                words.push(word);
                *self = Self::Wide(words);
            }
            Self::Wide(words) => words.push(word),
        }
    }

    fn pop(&mut self) {
        match self {
            Self::Inline { len, .. } => *len = len.saturating_sub(1),
            Self::Wide(words) => {
                words.pop();
            }
        }
    }

    fn remove_prefix(&mut self, count: usize) {
        match self {
            Self::Inline { data, len } => {
                data.copy_within(count..*len, 0);
                *len -= count;
            }
            Self::Wide(words) => {
                words.drain(..count);
            }
        }
    }
}

impl std::ops::Deref for Words {
    type Target = [u64];

    fn deref(&self) -> &Self::Target {
        match self {
            Self::Inline { data, len } => &data[..*len],
            Self::Wide(words) => words,
        }
    }
}

impl std::ops::DerefMut for Words {
    fn deref_mut(&mut self) -> &mut Self::Target {
        match self {
            Self::Inline { data, len } => &mut data[..*len],
            Self::Wide(words) => words,
        }
    }
}

impl PartialEq for Words {
    fn eq(&self, other: &Self) -> bool {
        **self == **other
    }
}

impl Eq for Words {}

impl FrameDuration {
    /// Resolve a duration without rounding its frame extent.
    pub fn new(duration: impl Into<Duration>, fps: u32) -> Result<Self, ClockError> {
        if fps == 0 {
            return Err(ClockError::ZeroFps);
        }
        let (value, seconds) = match duration.into() {
            Duration::Seconds(seconds) => {
                if !seconds.is_finite() {
                    return Err(ClockError::NonFiniteRunTime);
                }
                (
                    Dyadic::from_f64(seconds.max(0.0))?.mul_u64(u64::from(fps))?,
                    Some(seconds),
                )
            }
            Duration::Frames(frames) => {
                i64::try_from(frames).map_err(|_| ClockError::RunTimeTooLong)?;
                (Dyadic::integer(frames), None)
            }
        };
        Ok(Self {
            value,
            seconds,
            fps,
        })
    }

    /// Whether this duration was authored or composed in frame units.
    #[must_use]
    pub fn is_frame_native(&self) -> bool {
        self.seconds.is_none()
    }

    /// Seconds for interpolation or reporting only. Never feed this value
    /// back into the clock to recover a frame-native count.
    #[must_use]
    pub fn to_seconds(&self) -> f64 {
        self.seconds.unwrap_or_else(|| {
            self.value
                .quotient_f64(&Dyadic::integer(u64::from(self.fps)))
        })
    }

    /// The grid on which this duration was resolved.
    #[must_use]
    pub fn fps(&self) -> u32 {
        self.fps
    }

    /// The exact upward-rounded frame count.
    pub fn n_frames(&self) -> Result<i64, ClockError> {
        self.value.ceil()
    }

    /// Raw interpolation alpha at an integer frame coordinate. Compare the
    /// integer with the exact extent before projecting to f64, so a positive
    /// interior sample cannot round to an endpoint. Values beyond the extent
    /// remain unclamped. A zero extent retains IEEE division-by-zero results.
    #[must_use]
    pub fn sample_alpha(&self, frame: i64) -> f64 {
        if self.value.words.is_empty() {
            return frame as f64 / 0.0;
        }
        let magnitude = Dyadic::integer(frame.unsigned_abs());
        let alpha = magnitude.quotient_f64(&self.value);
        if frame > 0 && magnitude.cmp(&self.value).is_lt() {
            alpha.clamp(f64::from_bits(1), 1.0_f64.next_down())
        } else if frame < 0 {
            -alpha
        } else {
            alpha
        }
    }

    /// The unrounded duration as an exact position in frame units.
    #[must_use]
    pub fn as_position(&self) -> FramePosition {
        FramePosition {
            numerator: self.value.clone(),
            denominator: Dyadic::integer(1),
            negative: false,
        }
    }

    /// The unrounded frame extent as unsigned little-endian integer bytes.
    /// The returned pair is `(numerator, denominator)`, never a rounded count.
    /// This is a bounded host-language transport, not a sampling operation.
    #[must_use]
    pub fn ratio_parts(&self) -> (Vec<u8>, Vec<u8>) {
        self.value.ratio_parts()
    }

    /// Exact addition in frame units, with no intermediate ceiling.
    pub fn checked_add(&self, other: &Self) -> Result<Self, ClockError> {
        if self.fps != other.fps {
            return Err(ClockError::FrameRateMismatch);
        }
        Ok(Self {
            value: self.value.add(&other.value)?,
            seconds: None,
            fps: self.fps,
        })
    }

    /// Exact multiplication by a finite, nonnegative lag ratio.
    pub fn checked_scale(&self, factor: f64) -> Result<Self, ClockError> {
        if !factor.is_finite() || factor < 0.0 {
            return Err(ClockError::InvalidFrameLag);
        }
        let (mantissa, exponent, _) = decompose(factor);
        let mut value = self.value.mul_u64(mantissa)?;
        if !value.words.is_empty() {
            value.exponent = value
                .exponent
                .checked_add(exponent)
                .filter(|exponent| exponent.abs() <= MAX_DURATION_BITS)
                .ok_or(ClockError::DurationComplexity)?;
        }
        Ok(Self {
            value,
            seconds: None,
            fps: self.fps,
        })
    }

    /// Compare frame extents without converting either operand to seconds.
    /// Both values must belong to the same grid; composition and the play
    /// planner resolve every operand with their one receiving fps.
    #[must_use]
    pub fn cmp_duration(&self, other: &Self) -> std::cmp::Ordering {
        self.value.cmp(&other.value)
    }

    pub(crate) fn into_frame_native(mut self) -> Self {
        self.seconds = None;
        self
    }

    pub(crate) fn frame_units(&self) -> f64 {
        self.value.to_f64()
    }

    pub(crate) fn exact_parts(&self) -> (i32, &[u64]) {
        (self.value.exponent, &self.value.words)
    }

    pub(crate) fn from_exact_parts(
        fps: u32,
        exponent: i32,
        words: Vec<u64>,
    ) -> Result<Self, ClockError> {
        if fps == 0 {
            return Err(ClockError::ZeroFps);
        }
        if words.len() > MAX_DURATION_WORDS
            || !(-MAX_DURATION_BITS..=MAX_DURATION_BITS).contains(&exponent)
        {
            return Err(ClockError::DurationComplexity);
        }
        if (words.is_empty() && exponent != 0)
            || words.last().is_some_and(|word| *word == 0)
            || words.first().is_some_and(|word| word & 1 == 0)
        {
            return Err(ClockError::NonCanonicalDuration);
        }
        let duration = Self {
            value: Dyadic {
                words: Words::from_slice(&words),
                exponent,
            },
            seconds: None,
            fps,
        };
        duration.n_frames()?;
        Ok(duration)
    }
}

/// Nonnegative `words * 2^exponent`, with an odd, little-endian mantissa.
/// Duration planning and exact composition positions use the same arithmetic.
/// Ordinary sampling fits inline; unusually wide authored rationals retain
/// the bounded fallback used by duration planning.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Dyadic {
    words: Words,
    exponent: i32,
}

impl Dyadic {
    fn integer(value: u64) -> Self {
        let mut words = Words::with_capacity(1);
        if value != 0 {
            words.push(value);
        }
        let mut out = Self { words, exponent: 0 };
        out.normalize();
        out
    }

    fn from_f64(value: f64) -> Result<Self, ClockError> {
        if !value.is_finite() || value < 0.0 {
            return Err(ClockError::NonFiniteRunTime);
        }
        let (mantissa, exponent, _) = decompose(value);
        let mut out = Self::integer(mantissa);
        if !out.words.is_empty() {
            out.exponent += exponent;
        }
        Ok(out)
    }

    fn normalize(&mut self) {
        while self.words.last() == Some(&0) {
            self.words.pop();
        }
        if self.words.is_empty() {
            self.exponent = 0;
            return;
        }
        let whole = self.words.iter().take_while(|word| **word == 0).count();
        self.words.remove_prefix(whole);
        let bits = self.words[0].trailing_zeros();
        self.exponent += (whole * 64) as i32 + bits as i32;
        if bits != 0 {
            for index in 0..self.words.len() {
                self.words[index] = (self.words[index] >> bits)
                    | self
                        .words
                        .get(index + 1)
                        .copied()
                        .unwrap_or(0)
                        .wrapping_shl(64 - bits);
            }
            if self.words.last() == Some(&0) {
                self.words.pop();
            }
        }
    }

    fn bit_len(&self) -> i32 {
        self.words.last().map_or(0, |word| {
            ((self.words.len() - 1) * 64) as i32 + (64 - word.leading_zeros()) as i32
        })
    }

    fn bit(&self, index: usize) -> bool {
        self.words
            .get(index / 64)
            .is_some_and(|word| word & (1_u64 << (index % 64)) != 0)
    }

    fn shifted_word(&self, index: usize, shift: usize) -> u64 {
        let whole = shift / 64;
        let bits = shift % 64;
        let Some(index) = index.checked_sub(whole) else {
            return 0;
        };
        let low = self.words.get(index).copied().unwrap_or(0) << bits;
        if bits == 0 {
            low
        } else {
            low | index
                .checked_sub(1)
                .and_then(|i| self.words.get(i))
                .copied()
                .unwrap_or(0)
                >> (64 - bits)
        }
    }

    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        if self.words.is_empty() || other.words.is_empty() {
            return self.words.len().cmp(&other.words.len());
        }
        let top = self.bit_len() + self.exponent;
        let other_top = other.bit_len() + other.exponent;
        if top != other_top {
            return top.cmp(&other_top);
        }
        let base = self.exponent.min(other.exponent);
        let len = ((top - base + 63) / 64) as usize;
        for index in (0..len).rev() {
            let order = self
                .shifted_word(index, (self.exponent - base) as usize)
                .cmp(&other.shifted_word(index, (other.exponent - base) as usize));
            if !order.is_eq() {
                return order;
            }
        }
        std::cmp::Ordering::Equal
    }

    fn add(&self, other: &Self) -> Result<Self, ClockError> {
        if self.words.is_empty() {
            return Ok(other.clone());
        }
        if other.words.is_empty() {
            return Ok(self.clone());
        }
        let exponent = self.exponent.min(other.exponent);
        let top = (self.bit_len() + self.exponent).max(other.bit_len() + other.exponent);
        let len = ((top - exponent + 63) / 64) as usize;
        if len > MAX_DURATION_WORDS {
            return Err(ClockError::DurationComplexity);
        }
        let mut words = Words::with_capacity(len);
        let mut carry = 0_u128;
        for index in 0..len {
            let sum = u128::from(self.shifted_word(index, (self.exponent - exponent) as usize))
                + u128::from(other.shifted_word(index, (other.exponent - exponent) as usize))
                + carry;
            words.push(sum as u64);
            carry = sum >> 64;
        }
        if carry != 0 {
            if words.len() == MAX_DURATION_WORDS {
                return Err(ClockError::DurationComplexity);
            }
            words.push(carry as u64);
        }
        let mut out = Self { words, exponent };
        out.normalize();
        Ok(out)
    }

    fn mul_u64(&self, factor: u64) -> Result<Self, ClockError> {
        if factor == 0 {
            return Ok(Self::integer(0));
        }
        let mut words = Words::with_capacity(self.words.len());
        let mut carry = 0_u128;
        for word in self.words.iter() {
            let product = u128::from(*word) * u128::from(factor) + carry;
            words.push(product as u64);
            carry = product >> 64;
        }
        if carry != 0 {
            words.push(carry as u64);
        }
        if words.len() > MAX_DURATION_WORDS {
            return Err(ClockError::DurationComplexity);
        }
        let mut out = Self {
            words,
            exponent: self.exponent,
        };
        out.normalize();
        Ok(out)
    }

    fn bounded(self) -> Result<Self, ClockError> {
        if self.words.len() > MAX_DURATION_WORDS
            || !(-MAX_DURATION_BITS..=MAX_DURATION_BITS).contains(&self.exponent)
        {
            Err(ClockError::DurationComplexity)
        } else {
            Ok(self)
        }
    }

    fn from_bytes(bytes: &[u8]) -> Result<Self, ClockError> {
        if bytes.len() > FramePosition::MAX_RATIO_BYTES {
            return Err(ClockError::DurationComplexity);
        }
        let mut words = Words::with_capacity(bytes.len().div_ceil(8));
        for chunk in bytes.chunks(8) {
            let mut word = [0; 8];
            word[..chunk.len()].copy_from_slice(chunk);
            words.push(u64::from_le_bytes(word));
        }
        let mut out = Self { words, exponent: 0 };
        out.normalize();
        out.bounded()
    }

    fn unsigned_bytes(&self, shift: usize) -> Vec<u8> {
        let len = (self.bit_len() as usize + shift).div_ceil(8).max(1);
        (0..len)
            .map(|index| (self.shifted_word(index / 8, shift) >> (8 * (index % 8))) as u8)
            .collect()
    }

    fn ratio_parts(&self) -> (Vec<u8>, Vec<u8>) {
        if self.words.is_empty() {
            return (vec![0], vec![1]);
        }
        if self.exponent >= 0 {
            (self.unsigned_bytes(self.exponent as usize), vec![1])
        } else {
            let shift = (-self.exponent) as usize;
            let mut denominator = vec![0; shift / 8 + 1];
            denominator[shift / 8] = 1 << (shift % 8);
            (self.unsigned_bytes(0), denominator)
        }
    }

    /// Subtraction after the caller has established `self >= other`.
    fn subtract(&self, other: &Self) -> Result<Self, ClockError> {
        if other.words.is_empty() {
            return Ok(self.clone());
        }
        let exponent = self.exponent.min(other.exponent);
        let top = self.bit_len() + self.exponent;
        let len = ((top - exponent + 63) / 64) as usize;
        if len > MAX_DURATION_WORDS {
            return Err(ClockError::DurationComplexity);
        }
        let mut words = Words::with_capacity(len);
        let mut borrow = false;
        for index in 0..len {
            let a = self.shifted_word(index, (self.exponent - exponent) as usize);
            let b = other.shifted_word(index, (other.exponent - exponent) as usize);
            let (value, first) = a.overflowing_sub(b);
            let (value, second) = value.overflowing_sub(u64::from(borrow));
            words.push(value);
            borrow = first || second;
        }
        let mut out = Self { words, exponent };
        out.normalize();
        out.bounded()
    }

    fn multiply(&self, other: &Self) -> Result<Self, ClockError> {
        if self.words.is_empty() || other.words.is_empty() {
            return Ok(Self::integer(0));
        }
        if other.words.len() == 1 {
            let mut out = self.mul_u64(other.words[0])?;
            out.exponent = out
                .exponent
                .checked_add(other.exponent)
                .ok_or(ClockError::DurationComplexity)?;
            return out.bounded();
        }
        if self.words.len() == 1 {
            return other.multiply(self);
        }
        let len = self.words.len() + other.words.len();
        // One leading zero word may disappear after multiplication.
        if len > MAX_DURATION_WORDS + 1 {
            return Err(ClockError::DurationComplexity);
        }
        let mut words = Words::with_capacity(len);
        for _ in 0..len {
            words.push(0);
        }
        for (i, a) in self.words.iter().enumerate() {
            let mut carry = 0_u128;
            for (j, b) in other.words.iter().enumerate() {
                let product = u128::from(*a) * u128::from(*b) + u128::from(words[i + j]) + carry;
                words[i + j] = product as u64;
                carry = product >> 64;
            }
            words[i + other.words.len()] = carry as u64;
        }
        let exponent = self
            .exponent
            .checked_add(other.exponent)
            .ok_or(ClockError::DurationComplexity)?;
        let mut out = Self { words, exponent };
        out.normalize();
        out.bounded()
    }

    fn normalized_float(&self) -> (f64, i32) {
        let len = self.bit_len();
        if len == 0 {
            return (0.0, 0);
        }
        let mantissa = if len > 53 {
            self.rounded_shift((len - 53) as usize)
        } else {
            self.words[0] << (53 - len)
        };
        (
            mantissa as f64 / (1_u64 << 52) as f64,
            self.exponent + len - 1,
        )
    }

    fn quotient_f64(&self, denominator: &Self) -> f64 {
        if self.words.is_empty() {
            return 0.0;
        }
        if denominator.words.len() == 1 && denominator.words[0] == 1 && denominator.exponent == 0 {
            return self.to_f64();
        }
        let (numerator, numerator_exponent) = self.normalized_float();
        let (denominator, denominator_exponent) = denominator.normalized_float();
        let ratio = numerator / denominator;
        let exponent = numerator_exponent - denominator_exponent;
        if exponent > 1023 {
            (ratio * 2.0_f64.powi(1023)) * 2.0_f64.powi(exponent - 1023)
        } else if exponent < -1022 {
            (ratio * 2.0_f64.powi(-1022)) * 2.0_f64.powi(exponent + 1022)
        } else {
            ratio * 2.0_f64.powi(exponent)
        }
    }

    fn shr_u64(&self, shift: usize) -> u64 {
        let index = shift / 64;
        let bits = shift % 64;
        let low = self.words.get(index).copied().unwrap_or(0) >> bits;
        if bits == 0 {
            low
        } else {
            low | self.words.get(index + 1).copied().unwrap_or(0) << (64 - bits)
        }
    }

    fn rounded_shift(&self, shift: usize) -> u64 {
        let whole = self.shr_u64(shift);
        // The normalized mantissa is odd, so every discarded tail wider
        // than one bit has a sticky low bit.
        whole + u64::from(shift > 0 && self.bit(shift - 1) && (shift > 1 || whole & 1 != 0))
    }

    fn ceil(&self) -> Result<i64, ClockError> {
        if self.words.is_empty() {
            return Ok(0);
        }
        if self.bit_len() + self.exponent > 63 {
            return Err(ClockError::RunTimeTooLong);
        }
        let value = if self.exponent >= 0 {
            self.words[0] << self.exponent
        } else {
            self.shr_u64((-self.exponent) as usize) + 1
        };
        i64::try_from(value).map_err(|_| ClockError::RunTimeTooLong)
    }

    fn to_f64(&self) -> f64 {
        let len = self.bit_len();
        if len == 0 {
            return 0.0;
        }
        let mut top = self.exponent + len - 1;
        if top < -1022 {
            let shift = -(self.exponent + 1074);
            let bits = if shift >= 0 {
                self.rounded_shift(shift as usize)
            } else {
                self.words[0] << (-shift)
            };
            return f64::from_bits(bits);
        }
        let mut mantissa = if len > 53 {
            self.rounded_shift((len - 53) as usize)
        } else {
            self.words[0] << (53 - len)
        };
        if mantissa == 1_u64 << 53 {
            mantissa >>= 1;
            top += 1;
        }
        if top > 1023 {
            return f64::INFINITY;
        }
        f64::from_bits(((top + 1023) as u64) << 52 | (mantissa & 0x000f_ffff_ffff_ffff))
    }
}

/// An exact, signed rational position in frame units.
///
/// Integer clock samples and mixed-duration boundaries remain exact through
/// nested retiming. Floating-point conversion is reserved for interpolation
/// and reporting; it must never decide whether a child has started or ended.
/// Arithmetic shares [`FrameDuration`]'s bounded precision and returns a named
/// error when an authored expression exceeds it.
#[derive(Debug, Clone)]
pub struct FramePosition {
    numerator: Dyadic,
    denominator: Dyadic,
    negative: bool,
}

impl FramePosition {
    /// Maximum input bytes per integer for [`Self::from_ratio_bytes`]. The
    /// normalized significand and binary exponent each have a 16,384-bit budget.
    pub const MAX_RATIO_BYTES: usize = MAX_DURATION_WORDS * 16;

    /// Lift an integer frame without converting it to floating point.
    #[must_use]
    pub fn from_frame(frame: i64) -> Self {
        Self {
            numerator: Dyadic::integer(frame.unsigned_abs()),
            denominator: Dyadic::integer(1),
            negative: frame < 0,
        }
    }

    /// Lift the exact binary value of a finite manual frame position.
    pub fn from_f64(frame: f64) -> Result<Self, ClockError> {
        Self::new(
            Dyadic::from_f64(frame.abs())?,
            Dyadic::integer(1),
            frame < 0.0,
        )
    }

    /// Import an exact host-language fraction, with unsigned little-endian
    /// numerator/denominator magnitudes and an independent sign. A zero
    /// denominator or an oversized input is refused, never truncated.
    pub fn from_ratio_bytes(
        numerator_le: &[u8],
        denominator_le: &[u8],
        negative: bool,
    ) -> Result<Self, ClockError> {
        Self::new(
            Dyadic::from_bytes(numerator_le)?,
            Dyadic::from_bytes(denominator_le)?,
            negative,
        )
    }

    fn new(
        mut numerator: Dyadic,
        mut denominator: Dyadic,
        mut negative: bool,
    ) -> Result<Self, ClockError> {
        if denominator.words.is_empty() {
            return Err(ClockError::ZeroPositionDenominator);
        }
        if numerator.words.is_empty() {
            denominator = Dyadic::integer(1);
            negative = false;
        } else if numerator.words == denominator.words {
            numerator = Dyadic {
                words: Words::from_slice(&[1]),
                exponent: numerator
                    .exponent
                    .checked_sub(denominator.exponent)
                    .ok_or(ClockError::DurationComplexity)?,
            }
            .bounded()?;
            denominator = Dyadic::integer(1);
        } else if denominator.words.len() == 1 && denominator.words[0] == 1 {
            numerator.exponent = numerator
                .exponent
                .checked_sub(denominator.exponent)
                .ok_or(ClockError::DurationComplexity)?;
            numerator = numerator.bounded()?;
            denominator = Dyadic::integer(1);
        }
        Ok(Self {
            numerator,
            denominator,
            negative,
        })
    }

    /// Approximate interpolation/reporting value. Exact comparisons below do
    /// not use this conversion, including when both operands round identically.
    #[must_use]
    pub fn to_f64(&self) -> f64 {
        if self.numerator.words.is_empty() {
            return 0.0;
        }
        let magnitude = self.numerator.quotient_f64(&self.denominator);
        if self.negative { -magnitude } else { magnitude }
    }

    /// Exact comparison, including fractional tails smaller than an f64 ulp.
    pub fn checked_cmp(&self, other: &Self) -> Result<std::cmp::Ordering, ClockError> {
        if self.negative != other.negative {
            return Ok(other.negative.cmp(&self.negative));
        }
        let order = if self.denominator == other.denominator {
            self.numerator.cmp(&other.numerator)
        } else {
            self.numerator
                .multiply(&other.denominator)?
                .cmp(&other.numerator.multiply(&self.denominator)?)
        };
        Ok(if self.negative {
            order.reverse()
        } else {
            order
        })
    }

    /// Exact addition of positions, preserving the receiving frame units.
    pub fn checked_add(&self, other: &Self) -> Result<Self, ClockError> {
        let (left, right, denominator) = if self.denominator == other.denominator {
            (
                self.numerator.clone(),
                other.numerator.clone(),
                self.denominator.clone(),
            )
        } else {
            (
                self.numerator.multiply(&other.denominator)?,
                other.numerator.multiply(&self.denominator)?,
                self.denominator.multiply(&other.denominator)?,
            )
        };
        let (numerator, negative) = if self.negative == other.negative {
            (left.add(&right)?.bounded()?, self.negative)
        } else if left.cmp(&right).is_lt() {
            (right.subtract(&left)?, other.negative)
        } else {
            (left.subtract(&right)?, self.negative)
        };
        Self::new(numerator, denominator, negative)
    }

    /// Exact subtraction. Negative local/manual positions remain representable.
    pub fn checked_subtract(&self, other: &Self) -> Result<Self, ClockError> {
        let mut opposite = other.clone();
        if !opposite.numerator.words.is_empty() {
            opposite.negative = !opposite.negative;
        }
        self.checked_add(&opposite)
    }

    /// Exact multiplication, cancelling identical factors before growing them.
    pub fn checked_multiply(&self, other: &Self) -> Result<Self, ClockError> {
        let cancel_left = self.numerator == other.denominator;
        let cancel_right = other.numerator == self.denominator;
        let numerator = match (cancel_left, cancel_right) {
            (true, true) => Dyadic::integer(1),
            (true, false) => other.numerator.clone(),
            (false, true) => self.numerator.clone(),
            (false, false) => self.numerator.multiply(&other.numerator)?,
        };
        let denominator = match (cancel_left, cancel_right) {
            (true, true) => Dyadic::integer(1),
            (true, false) => self.denominator.clone(),
            (false, true) => other.denominator.clone(),
            (false, false) => self.denominator.multiply(&other.denominator)?,
        };
        Self::new(numerator, denominator, self.negative != other.negative)
    }

    /// Exact division; a zero divisor is a named error.
    pub fn checked_divide(&self, other: &Self) -> Result<Self, ClockError> {
        if other.numerator.words.is_empty() {
            return Err(ClockError::ZeroPositionDenominator);
        }
        self.checked_multiply(&Self {
            numerator: other.denominator.clone(),
            denominator: other.numerator.clone(),
            negative: other.negative,
        })
    }

    pub(crate) fn clamp_unit(&self) -> Result<Self, ClockError> {
        let zero = Self::from_frame(0);
        let one = Self::from_frame(1);
        Ok(if self.checked_cmp(&zero)?.is_lt() {
            zero
        } else if self.checked_cmp(&one)?.is_gt() {
            one
        } else {
            self.clone()
        })
    }

    // Keep an exact interior point interior when the nearest floating-point
    // alpha is an endpoint. This is endpoint classification, not an epsilon
    // used to move authored boundaries or change ceiling semantics.
    pub(crate) fn interpolation_alpha(&self) -> Result<f64, ClockError> {
        let value = self.to_f64();
        if self.checked_cmp(&Self::from_frame(0))?.is_gt()
            && self.checked_cmp(&Self::from_frame(1))?.is_lt()
        {
            Ok(value.clamp(f64::from_bits(1), 1.0_f64.next_down()))
        } else {
            Ok(value)
        }
    }
}

/// Exact rational time: `frames / fps` seconds. The only time values the
/// engine can express are on the frame grid — off-grid time is
/// unrepresentable by construction.
#[derive(Debug, Clone, Copy)]
pub struct RationalTime {
    frames: i64,
    fps: u32,
}

impl RationalTime {
    /// The zero instant on a given grid.
    #[must_use]
    pub fn zero(fps: u32) -> Self {
        Self { frames: 0, fps }
    }

    #[must_use]
    pub fn frames(&self) -> i64 {
        self.frames
    }

    #[must_use]
    pub fn fps(&self) -> u32 {
        self.fps
    }

    /// The value in seconds, converted with a single division — the one
    /// rounding at the consumer boundary (§6.1).
    #[must_use]
    pub fn to_f64(&self) -> f64 {
        self.frames as f64 / f64::from(self.fps)
    }

    /// Exact comparison against an `f64` number of seconds (no rounding:
    /// cross-multiplied in wide integers).
    #[must_use]
    pub fn cmp_seconds(&self, seconds: f64) -> Option<std::cmp::Ordering> {
        if !seconds.is_finite() {
            return None;
        }
        // self.frames / fps  vs  m·2^e  ⇔  frames·2^-e vs m·fps (e < 0).
        let (m, e, sign) = decompose(seconds);
        let lhs_sign = self.frames.signum();
        let rhs_sign = if m == 0 { 0 } else { sign };
        if lhs_sign != rhs_sign {
            return Some(lhs_sign.cmp(&(rhs_sign)));
        }
        if lhs_sign == 0 && rhs_sign == 0 {
            return Some(std::cmp::Ordering::Equal);
        }
        // Compare |frames|·2^max(-e,0) vs m·fps·2^max(e,0), then apply sign.
        let lhs = i128::from(self.frames.unsigned_abs());
        let rhs = i128::from(m) * i128::from(self.fps);
        let ord = shifted_cmp(lhs, rhs, e);
        Some(if lhs_sign < 0 { ord.reverse() } else { ord })
    }
}

impl std::ops::Add<i64> for RationalTime {
    type Output = Self;

    /// Advance by whole frames — the only arithmetic the grid admits.
    fn add(self, frames: i64) -> Self {
        Self {
            frames: self.frames + frames,
            fps: self.fps,
        }
    }
}

impl PartialEq for RationalTime {
    fn eq(&self, other: &Self) -> bool {
        i128::from(self.frames) * i128::from(other.fps)
            == i128::from(other.frames) * i128::from(self.fps)
    }
}

impl Eq for RationalTime {}

impl PartialOrd for RationalTime {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for RationalTime {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        (i128::from(self.frames) * i128::from(other.fps))
            .cmp(&(i128::from(other.frames) * i128::from(self.fps)))
    }
}

/// (mantissa, exponent, sign) with value = sign · m · 2^e for finite x.
fn decompose(x: f64) -> (u64, i32, i64) {
    let bits = x.to_bits();
    let sign = if bits >> 63 == 1 { -1 } else { 1 };
    let biased = ((bits >> 52) & 0x7ff) as i32;
    let frac = bits & 0x000f_ffff_ffff_ffff;
    if biased == 0 {
        (frac, -1074, sign)
    } else {
        (frac | (1 << 52), biased - 1075, sign)
    }
}

/// Compare `a` vs `b · 2^k` for positive `a`, `b` and any `k ≥ 0`,
/// overflow-free: bit lengths decide unless they tie, and a tie means the
/// shift fits in i128.
fn cmp_with_shift(a: i128, b: i128, k: u32) -> std::cmp::Ordering {
    let len_a = 128 - a.leading_zeros();
    let len_b = (128 - b.leading_zeros()).saturating_add(k);
    if len_a != len_b {
        return len_a.cmp(&len_b);
    }
    // Equal bit lengths: len_b ≤ 127, so b << k cannot overflow.
    a.cmp(&(b << k))
}

/// Compare `lhs` vs `rhs · 2^e` for positive operands and any f64 exponent.
fn shifted_cmp(lhs: i128, rhs: i128, e: i32) -> std::cmp::Ordering {
    if e >= 0 {
        cmp_with_shift(lhs, rhs, e as u32)
    } else {
        cmp_with_shift(rhs, lhs, (-e) as u32).reverse()
    }
}

/// Errors from the clock.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClockError {
    /// fps must be nonzero.
    ZeroFps,
    /// run_time must be finite (NaN/±inf refused, never silently coerced).
    NonFiniteRunTime,
    /// The requested duration exceeds what an i64 frame index can count.
    RunTimeTooLong,
    /// A clock can only advance, never move to a negative earlier frame.
    NegativeFrameAdvance,
    /// The current frame plus an otherwise valid advance exceeds `i64`.
    FrameCounterOverflow,
    /// Frame-native composition requires a finite, nonnegative lag ratio.
    InvalidFrameLag,
    /// Exact duration arithmetic exceeded its bounded precision budget.
    DurationComplexity,
    /// An encoded exact duration was not in its unique normalized form.
    NonCanonicalDuration,
    /// A resolved duration belongs to another frame grid.
    FrameRateMismatch,
    /// An exact position cannot have a zero denominator.
    ZeroPositionDenominator,
}

impl std::fmt::Display for ClockError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ZeroFps => write!(f, "fps must be nonzero"),
            Self::NonFiniteRunTime => write!(f, "run_time must be finite"),
            Self::RunTimeTooLong => write!(f, "run_time exceeds the frame counter's range"),
            Self::NegativeFrameAdvance => write!(f, "frame advance must be non-negative"),
            Self::FrameCounterOverflow => write!(f, "frame advance exceeds the counter's range"),
            Self::InvalidFrameLag => {
                write!(f, "frame-native lag_ratio must be finite and non-negative")
            }
            Self::DurationComplexity => {
                write!(f, "exact duration exceeds the 16384-bit arithmetic budget")
            }
            Self::NonCanonicalDuration => write!(f, "exact duration is not canonically normalized"),
            Self::FrameRateMismatch => {
                write!(f, "resolved duration belongs to a different frame rate")
            }
            Self::ZeroPositionDenominator => {
                write!(f, "exact frame position denominator must be nonzero")
            }
        }
    }
}

impl std::error::Error for ClockError {}

/// `ceil(run_time · fps)` on the exact rational value of the f64 —
/// the segment frame count (0 for nonpositive durations).
fn frames_covering(run_time: f64, fps: u32) -> Result<i64, ClockError> {
    if !run_time.is_finite() {
        return Err(ClockError::NonFiniteRunTime);
    }
    if run_time <= 0.0 {
        return Ok(0);
    }
    let (m, e, _) = decompose(run_time);
    let product = i128::from(m) * i128::from(fps);
    if product == 0 {
        return Ok(0);
    }
    let n = if e >= 0 {
        if e >= 40 || (product << e) > i128::from(i64::MAX) {
            return Err(ClockError::RunTimeTooLong);
        }
        product << e
    } else {
        let s = -e;
        if s >= 127 {
            1 // a positive duration below every grid step still takes one frame
        } else {
            ((product - 1) >> s) + 1
        }
    };
    i64::try_from(n).map_err(|_| ClockError::RunTimeTooLong)
}

/// One emitted sample of a segment.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FrameSample {
    /// 1-based frame number within the segment (there is no zero-alpha
    /// emission frame).
    pub frame: i64,
    /// Exact segment-local time `frame / fps`.
    pub time: RationalTime,
    /// `time / run_time`, clamped to `[0, 1]` (the Reference's clip).
    pub alpha: f64,
}

/// The sampling plan for one `play()`/`wait()` segment.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FrameSegment {
    fps: u32,
    n_frames: i64,
    run_time: f64,
    frame_units: Option<f64>,
}

impl FrameSegment {
    /// Number of frames the segment emits.
    #[must_use]
    pub fn n_frames(&self) -> i64 {
        self.n_frames
    }

    /// The requested duration in seconds (as received; the grid end may
    /// exceed it — upward rounding).
    #[must_use]
    pub fn run_time(&self) -> f64 {
        self.run_time
    }

    /// The exact grid time of the segment's end (`n_frames / fps`), which
    /// is `>= run_time` for positive durations.
    #[must_use]
    pub fn end_time(&self) -> RationalTime {
        RationalTime {
            frames: self.n_frames,
            fps: self.fps,
        }
    }

    /// Return one 1-based sample without walking the preceding frame grid.
    ///
    /// Resumable front ends use this to prepare exactly one frame, release
    /// their arena borrow for a host-language updater phase, then resume the
    /// same sample for capture. Keeping the lookup O(1) avoids turning that
    /// release window into quadratic segment work.
    #[must_use]
    pub fn sample(&self, frame: i64) -> Option<FrameSample> {
        if !(1..=self.n_frames).contains(&frame) {
            return None;
        }
        let time = RationalTime {
            frames: frame,
            fps: self.fps,
        };
        let raw = self.frame_units.map_or_else(
            || time.to_f64() / self.run_time,
            |duration| frame as f64 / duration,
        );
        let alpha = raw.clamp(0.0, 1.0);
        // The exact ceiling proves every earlier integer frame is strictly
        // inside a frame-native extent, even if its float projection rounds
        // down onto that frame. Retain Copy/O(1) grid lookup without storing
        // a second arbitrary-precision duration inside the clock segment.
        let alpha = if self.frame_units.is_some() {
            if frame < self.n_frames {
                alpha.min(1.0_f64.next_down())
            } else {
                1.0
            }
        } else {
            alpha
        };
        Some(FrameSample { frame, time, alpha })
    }

    /// The emitted samples, in frame order. Alphas are nondecreasing, the
    /// first is strictly positive, the last is exactly 1 whenever the grid
    /// end reaches or exceeds `run_time`.
    pub fn samples(&self) -> impl Iterator<Item = FrameSample> + '_ {
        (1..=self.n_frames).map(|frame| {
            self.sample(frame)
                .expect("the iterator only produces in-range frame indices")
        })
    }
}

/// The clock: an i64 frame counter over fps. Time derives; it never
/// accumulates.
#[derive(Debug, Clone, Copy)]
pub struct RationalFrameClock {
    fps: u32,
    frames_elapsed: i64,
}

impl RationalFrameClock {
    pub fn new(fps: u32) -> Result<Self, ClockError> {
        if fps == 0 {
            return Err(ClockError::ZeroFps);
        }
        Ok(Self {
            fps,
            frames_elapsed: 0,
        })
    }

    #[must_use]
    pub fn fps(&self) -> u32 {
        self.fps
    }

    /// The current scene time — exact, derived, drift-free.
    #[must_use]
    pub fn now(&self) -> RationalTime {
        RationalTime {
            frames: self.frames_elapsed,
            fps: self.fps,
        }
    }

    /// The frame step, exactly `1 / fps`.
    #[must_use]
    pub fn dt(&self) -> RationalTime {
        RationalTime {
            frames: 1,
            fps: self.fps,
        }
    }

    /// Advance by emitted frames (one per capture; a consumer that stops a
    /// wait early — `wait_until` — simply advances by fewer).
    ///
    /// # Errors
    /// [`ClockError::NegativeFrameAdvance`] for a negative delta, or
    /// [`ClockError::FrameCounterOverflow`] if the resulting frame cannot be
    /// represented.
    pub fn advance_frames(&mut self, n: i64) -> Result<(), ClockError> {
        if n < 0 {
            return Err(ClockError::NegativeFrameAdvance);
        }
        self.frames_elapsed = self
            .frames_elapsed
            .checked_add(n)
            .ok_or(ClockError::FrameCounterOverflow)?;
        Ok(())
    }

    /// Plan a segment of `run_time` seconds on this clock's grid.
    pub fn segment(&self, duration: impl Into<Duration>) -> Result<FrameSegment, ClockError> {
        let (n_frames, run_time, frame_units) = match duration.into() {
            Duration::Seconds(seconds) => (frames_covering(seconds, self.fps)?, seconds, None),
            Duration::Frames(frames) => {
                let n_frames = i64::try_from(frames).map_err(|_| ClockError::RunTimeTooLong)?;
                (
                    n_frames,
                    frames as f64 / f64::from(self.fps),
                    Some(frames as f64),
                )
            }
        };
        self.plan_count(n_frames, run_time, frame_units)
    }

    /// Plan exactly `frames` samples without converting the count to seconds.
    pub fn segment_frames(&self, frames: u64) -> Result<FrameSegment, ClockError> {
        self.segment(Duration::Frames(frames))
    }

    /// Plan a previously resolved duration, including a composition's exact
    /// fractional frame extent. The only ceiling occurs here.
    pub fn segment_duration(&self, duration: &FrameDuration) -> Result<FrameSegment, ClockError> {
        if duration.fps() != self.fps {
            return Err(ClockError::FrameRateMismatch);
        }
        if let Some(seconds) = duration.seconds {
            return self.segment(seconds);
        }
        self.plan_count(
            duration.n_frames()?,
            duration.to_seconds(),
            Some(duration.frame_units()),
        )
    }

    fn plan_count(
        &self,
        n_frames: i64,
        run_time: f64,
        frame_units: Option<f64>,
    ) -> Result<FrameSegment, ClockError> {
        self.frames_elapsed
            .checked_add(n_frames)
            .ok_or(ClockError::FrameCounterOverflow)?;
        Ok(FrameSegment {
            fps: self.fps,
            n_frames,
            run_time,
            frame_units,
        })
    }
}
