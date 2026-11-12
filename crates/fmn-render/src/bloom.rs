//! Controlled bloom over a raw linear-light frame.
//!
//! Each pixel's light is spread by a separable binomial blur and mixed back
//! convexly, per channel of the premultiplied `Rgba16F` frame:
//!
//! ```text
//! out = in × (1 − s) + blur(in) × s
//! ```
//!
//! with `s` the strength in `[0, 1]`. Because the taps sum to one and the
//! mix is convex, a channel never rises above the brightest value of that
//! channel within the blur's reach (dense light does not run to white), a
//! premultiplied colour stays within its alpha, a thin dark boundary
//! between bright regions keeps at least `1 − 2s` of its contrast, and
//! strength `0` returns the frame unchanged.
//!
//! The bits are a pure function of the frame and the bloom. The taps are
//! binomial coefficients computed with IEEE basic operations only (no
//! platform `exp`); every sum runs in ascending tap order with a separate
//! multiply and add (no FMA); the row pass keeps `f32` sums; edges clamp;
//! the result is encoded round-to-nearest-even. Rows are write-disjoint, so
//! the thread count changes nothing. The Metal route's two kernels run the
//! same arithmetic in the same order.
//!
//! The frame blooms in bands of [`BLOOM_BAND_ROWS`] rows, top to bottom, so
//! only the row sums within the blur's reach of one band are held at once,
//! in a ring ([`Bloom::ring_rows`]): before a band is mixed in place, the
//! sums of every row it reaches are taken from rows not yet written, each
//! row's once. The bits do not depend on the band height either.

use std::fmt;
use std::sync::{Mutex, PoisonError};

use fmn_frame::half::{f16_from_f32, f16_to_f32};
use fmn_frame::{FrameBuffer, FrameError, PixelFormat};

use crate::engine::{NativeScopedSpawner, run_scoped_workers};

/// The widest bloom accepted: its standard deviation in pixels.
pub const BLOOM_SIGMA_MAX_PIXELS: f64 = 32.0;

/// Bytes per pixel of the row sums a blooming frame holds: four `f32`
/// sums, for the rows of [`Bloom::ring_rows`].
pub const BLOOM_SCRATCH_BYTES_PER_PIXEL: u64 = 16;

/// The rows a frame blooms in at once: the band the column pass mixes
/// before the next band's row sums replace the ones it no longer reaches.
pub const BLOOM_BAND_ROWS: u32 = 128;

/// A refused bloom.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum BloomError {
    /// The strength is not within `[0, 1]`.
    Strength(f64),
    /// The width is not within `(0, BLOOM_SIGMA_MAX_PIXELS]` pixels.
    Sigma(f64),
}

impl fmt::Display for BloomError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Strength(strength) => {
                write!(f, "bloom strength {strength} is outside [0, 1]")
            }
            Self::Sigma(sigma) => write!(
                f,
                "bloom width {sigma} pixels is outside (0, {BLOOM_SIGMA_MAX_PIXELS}]"
            ),
        }
    }
}

impl std::error::Error for BloomError {}

/// A controlled bloom: its strength and its blur's taps.
#[derive(Debug, Clone, PartialEq)]
pub struct Bloom {
    strength: f32,
    keep: f32,
    taps: Vec<f32>,
}

impl Bloom {
    /// A bloom of `strength` in `[0, 1]` whose blur has a standard deviation
    /// of about `sigma_pixels`: binomial taps of order `n = round(2σ²)` (at
    /// least 1), whose deviation is `√(n / 2)`, cut at `⌈3σ⌉` pixels (or `n`
    /// when smaller) and normalized.
    ///
    /// # Errors
    /// [`BloomError::Strength`] for a strength outside `[0, 1]` (or NaN);
    /// [`BloomError::Sigma`] for a width outside
    /// `(0, BLOOM_SIGMA_MAX_PIXELS]` (or NaN).
    pub fn new(strength: f64, sigma_pixels: f64) -> Result<Self, BloomError> {
        if !(0.0..=1.0).contains(&strength) {
            return Err(BloomError::Strength(strength));
        }
        if !(sigma_pixels > 0.0 && sigma_pixels <= BLOOM_SIGMA_MAX_PIXELS) {
            return Err(BloomError::Sigma(sigma_pixels));
        }
        let order = (2.0 * sigma_pixels * sigma_pixels).round().max(1.0);
        let radius = (3.0 * sigma_pixels).ceil().min(order);
        // Both are small whole numbers: at most 2,048 and 96.
        let order = order as u32;
        let radius = radius as u32;
        // C(2n, n + k) / C(2n, n), from the centre outwards.
        let mut relative = Vec::with_capacity(radius as usize + 1);
        let mut weight = 1.0_f64;
        relative.push(weight);
        for k in 0..radius {
            weight = weight * f64::from(order - k) / f64::from(order + k + 1);
            relative.push(weight);
        }
        let mut sides = 0.0_f64;
        for &weight in &relative[1..] {
            sides += weight;
        }
        let total = relative[0] + 2.0 * sides;
        let mut taps = Vec::with_capacity(2 * radius as usize + 1);
        for &weight in relative.iter().rev() {
            taps.push((weight / total) as f32);
        }
        for &weight in &relative[1..] {
            taps.push((weight / total) as f32);
        }
        Ok(Self {
            strength: strength as f32,
            keep: (1.0 - strength) as f32,
            taps,
        })
    }

    /// The mix's strength.
    #[must_use]
    pub const fn strength(&self) -> f32 {
        self.strength
    }

    /// The input's share of the mix, `1 − strength`.
    #[must_use]
    pub const fn keep(&self) -> f32 {
        self.keep
    }

    /// The blur's taps, from `-radius` to `radius` pixels.
    #[must_use]
    pub fn taps(&self) -> &[f32] {
        &self.taps
    }

    /// The blur's reach in pixels on each side.
    #[must_use]
    pub fn radius(&self) -> usize {
        self.taps.len() / 2
    }

    /// The rows of row sums a frame `height` rows tall holds while it
    /// blooms: a band of [`BLOOM_BAND_ROWS`] and the blur's reach above and
    /// below it, or the whole frame when that is shorter.
    #[must_use]
    pub fn ring_rows(&self, height: u32) -> u32 {
        ring_rows(height, BLOOM_BAND_ROWS, self.radius())
    }

    /// The bytes of row sums a `width` × `height` frame holds while it
    /// blooms: [`BLOOM_SCRATCH_BYTES_PER_PIXEL`] for each pixel of
    /// [`Bloom::ring_rows`] rows. [`Bloom::apply`] allocates them; the Metal
    /// route keeps them on the device. `None` on overflow.
    #[must_use]
    pub fn scratch_bytes(&self, width: u32, height: u32) -> Option<u64> {
        u64::from(width)
            .checked_mul(u64::from(self.ring_rows(height)))?
            .checked_mul(BLOOM_SCRATCH_BYTES_PER_PIXEL)
    }

    /// Bloom a raw `Rgba16F` frame in place on up to `threads` threads.
    ///
    /// # Errors
    /// [`FrameError::FormatMismatch`] for another format, the frame
    /// untouched; [`FrameError::TooLarge`] when the row sums cannot be
    /// reserved, the frame untouched; [`FrameError::WorkerSpawnFailed`] if
    /// the host cannot start the requested team, which leaves the bands
    /// before it bloomed and the rest not.
    pub fn apply(&self, frame: &mut FrameBuffer, threads: usize) -> Result<(), FrameError> {
        self.apply_in_bands(frame, threads, BLOOM_BAND_ROWS)
    }

    /// [`Bloom::apply`] in bands of `band` rows (at least one).
    fn apply_in_bands(
        &self,
        frame: &mut FrameBuffer,
        threads: usize,
        band: u32,
    ) -> Result<(), FrameError> {
        let layout = frame.layout();
        if layout.format() != PixelFormat::Rgba16F {
            return Err(FrameError::FormatMismatch {
                expected: "Rgba16F raw frame",
                got: layout.format(),
            });
        }
        let width = layout.width() as usize;
        let height = layout.height() as usize;
        let stride = layout.stride(0);
        let band = band.max(1) as usize;
        let radius = self.radius();
        let ring = ring_rows(layout.height(), band as u32, radius) as usize;
        let row_values = width.checked_mul(4).ok_or(FrameError::TooLarge)?;
        let values = row_values.checked_mul(ring).ok_or(FrameError::TooLarge)?;
        let mut sums = Vec::new();
        sums.try_reserve_exact(values)
            .map_err(|_| FrameError::TooLarge)?;
        sums.resize(values, 0.0_f32);
        let workers = crate::effective_threads(threads).max(1);

        // Rows `0..summed` have had their sums taken; row `y`'s are in slot
        // `y % ring`. A band's rows reach `radius` rows past it, so their
        // sums are taken first, from rows no band has written yet; the slots
        // they take hold rows above the band's reach.
        let mut summed = 0;
        let mut first = 0;
        while first < height {
            let last = height.min(first + band);
            let ahead = height.min(last + radius);
            if ahead > summed {
                let (from, plane) = (summed, frame.plane(0));
                for_each_row(workers, &mut sums, row_values, |slot, row| {
                    let y = from + (slot + ring - from % ring) % ring;
                    if y < ahead {
                        self.blur_row(&plane[y * stride..], width, row);
                    }
                })?;
                summed = ahead;
            }
            let sums = &sums;
            let plane = frame.plane_mut(0);
            let end = plane.len().min(last * stride);
            for_each_row(
                workers,
                &mut plane[first * stride..end],
                stride,
                |i, row| {
                    self.blur_column_and_mix(sums, ring, first + i, width, height, row);
                },
            )?;
            first = last;
        }
        Ok(())
    }

    /// The row pass: `sums[x] = Σₖ taps[k] × in[clamp(x + k − r)]`, per
    /// channel, in ascending `k`.
    fn blur_row(&self, source: &[u8], width: usize, sums: &mut [f32]) {
        let radius = self.radius();
        for x in 0..width {
            let mut acc = [0.0_f32; 4];
            for (k, &tap) in self.taps.iter().enumerate() {
                let sx = (x + k).saturating_sub(radius).min(width - 1);
                for (c, acc) in acc.iter_mut().enumerate() {
                    *acc += tap * channel(source, sx * 4 + c);
                }
            }
            sums[x * 4..x * 4 + 4].copy_from_slice(&acc);
        }
    }

    /// The column pass over the row sums, held in a ring of `ring` rows,
    /// then the mix with the frame's own row `y`, written back in place.
    fn blur_column_and_mix(
        &self,
        sums: &[f32],
        ring: usize,
        y: usize,
        width: usize,
        height: usize,
        row: &mut [u8],
    ) {
        let radius = self.radius();
        let row_values = width * 4;
        for x in 0..width {
            let mut acc = [0.0_f32; 4];
            for (k, &tap) in self.taps.iter().enumerate() {
                let sy = (y + k).saturating_sub(radius).min(height - 1);
                let base = (sy % ring) * row_values + x * 4;
                for (c, acc) in acc.iter_mut().enumerate() {
                    *acc += tap * sums[base + c];
                }
            }
            for (c, &blurred) in acc.iter().enumerate() {
                let at = (x * 4 + c) * 2;
                let mixed = channel(row, x * 4 + c) * self.keep + blurred * self.strength;
                row[at..at + 2].copy_from_slice(&f16_from_f32(mixed).to_le_bytes());
            }
        }
    }
}

/// The rows of row sums a frame `height` rows tall blooming in bands of
/// `band` rows with a blur of `radius` holds: the band and the blur's reach
/// above and below it, or the whole frame when that is shorter.
pub(crate) fn ring_rows(height: u32, band: u32, radius: usize) -> u32 {
    // The radius is at most 96 (`BLOOM_SIGMA_MAX_PIXELS`).
    let reach = band.saturating_add(2 * radius as u32);
    height.min(reach)
}

/// Channel `index` of a row of little-endian binary16 samples.
fn channel(row: &[u8], index: usize) -> f32 {
    f16_to_f32(u16::from_le_bytes([row[index * 2], row[index * 2 + 1]]))
}

/// Run `work(y, row)` over the `row_len`-element rows of `data` on up to
/// `workers` threads. Rows are write-disjoint, so the order they run in
/// changes nothing.
fn for_each_row<T, F>(
    workers: usize,
    data: &mut [T],
    row_len: usize,
    work: F,
) -> Result<(), FrameError>
where
    T: Send,
    F: Fn(usize, &mut [T]) + Sync,
{
    if workers == 1 {
        for (y, row) in data.chunks_mut(row_len).enumerate() {
            work(y, row);
        }
        return Ok(());
    }
    let queue = Mutex::new(data.chunks_mut(row_len).enumerate());
    run_scoped_workers(workers, &NativeScopedSpawner, || {
        loop {
            let next = queue.lock().unwrap_or_else(PoisonError::into_inner).next();
            let Some((y, row)) = next else { break };
            work(y, row);
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use fmn_frame::FrameLayout;

    fn frame(width: u32, height: u32, pixel: impl Fn(u32, u32) -> [f32; 4]) -> FrameBuffer {
        let layout = FrameLayout::tight(PixelFormat::Rgba16F, width, height).expect("layout");
        let mut frame = FrameBuffer::new(layout);
        let stride = frame.layout().stride(0);
        let plane = frame.plane_mut(0);
        for y in 0..height {
            for x in 0..width {
                for (c, value) in pixel(x, y).into_iter().enumerate() {
                    let at = y as usize * stride + (x as usize * 4 + c) * 2;
                    plane[at..at + 2].copy_from_slice(&f16_from_f32(value).to_le_bytes());
                }
            }
        }
        frame
    }

    fn value(frame: &FrameBuffer, x: u32, y: u32, c: usize) -> f32 {
        let stride = frame.layout().stride(0);
        channel(&frame.plane(0)[y as usize * stride..], x as usize * 4 + c)
    }

    /// Premultiplied colour from a deterministic hash: alpha in [0, 1],
    /// colour within it, a few bright pixels.
    fn speckle(x: u32, y: u32) -> [f32; 4] {
        let h =
            (x.wrapping_mul(73_856_093) ^ y.wrapping_mul(19_349_663)).wrapping_mul(2_654_435_761);
        let unit = |shift: u32| ((h >> shift) & 0xff) as f32 / 255.0;
        let alpha = if h.is_multiple_of(7) { 1.0 } else { unit(0) };
        [unit(8) * alpha, unit(16) * alpha, unit(24) * alpha, alpha]
    }

    #[test]
    fn the_taps_are_the_normalized_binomial_cut_at_three_deviations() {
        // σ = 1: order 2, radius min(3, 2) = 2: [1, 4, 6, 4, 1] / 16, exactly.
        let bloom = Bloom::new(0.15, 1.0).expect("bloom");
        assert_eq!(bloom.taps(), &[0.0625, 0.25, 0.375, 0.25, 0.0625]);
        assert_eq!(bloom.radius(), 2);
        // σ = 3: order 18, cut at 9; symmetric, summing to one in f64 within
        // the f32 rounding of each tap, largest at the centre.
        let bloom = Bloom::new(0.15, 3.0).expect("bloom");
        let taps = bloom.taps();
        assert_eq!(taps.len(), 19);
        assert!(taps.iter().zip(taps.iter().rev()).all(|(a, b)| a == b));
        let sum: f64 = taps.iter().map(|&tap| f64::from(tap)).sum();
        assert!((sum - 1.0).abs() < 1e-6, "{sum}");
        assert!(taps.windows(2).take(9).all(|pair| pair[0] < pair[1]));
        // Its deviation is √(order / 2) = 3 before the cut.
        let variance: f64 = taps
            .iter()
            .enumerate()
            .map(|(k, &tap)| f64::from(tap) * (k as f64 - 9.0).powi(2))
            .sum();
        assert!((variance.sqrt() - 3.0).abs() < 0.05, "{variance}");
        assert_eq!((bloom.strength(), bloom.keep()), (0.15, 0.85));
    }

    #[test]
    fn the_bits_do_not_depend_on_the_band_height() {
        // One band of the whole frame keeps every row's sums at once (the
        // ring is the frame); every other height evicts sums the bands
        // above no longer reach, and must give the same bits.
        let original = frame(29, 31, speckle);
        for sigma in [0.7, 1.5, 3.0] {
            let bloom = Bloom::new(0.4, sigma).expect("bloom");
            let mut whole = original.clone();
            bloom.apply_in_bands(&mut whole, 1, 31).expect("blooms");
            assert_ne!(whole.as_bytes(), original.as_bytes());
            for band in [1, 2, 5, 9, 30, BLOOM_BAND_ROWS] {
                for threads in [1, 3] {
                    let mut banded = original.clone();
                    bloom
                        .apply_in_bands(&mut banded, threads, band)
                        .expect("blooms");
                    assert_eq!(
                        banded.as_bytes(),
                        whole.as_bytes(),
                        "{sigma} {band} {threads}"
                    );
                }
            }
        }
    }

    #[test]
    fn a_bloom_holds_the_row_sums_of_one_band_and_its_reach() {
        // sigma 3: radius 9. A 4K frame holds 128 + 18 rows of sums, 9 MB
        // where the whole frame's would be 133 MB; a frame shorter than
        // that holds its own rows.
        let bloom = Bloom::new(0.15, 3.0).expect("bloom");
        assert_eq!(bloom.ring_rows(2160), 146);
        assert_eq!(bloom.scratch_bytes(3840, 2160), Some(3840 * 146 * 16));
        assert_eq!(bloom.ring_rows(100), 100);
        assert_eq!(bloom.scratch_bytes(64, 100), Some(64 * 100 * 16));
        let widest = Bloom::new(0.15, BLOOM_SIGMA_MAX_PIXELS).expect("bloom");
        assert_eq!(widest.radius(), 96);
        assert_eq!(widest.ring_rows(2160), 128 + 192);
    }

    #[test]
    fn a_bloom_outside_its_bounds_is_refused() {
        assert_eq!(Bloom::new(-0.1, 2.0), Err(BloomError::Strength(-0.1)));
        assert_eq!(Bloom::new(1.5, 2.0), Err(BloomError::Strength(1.5)));
        assert!(matches!(
            Bloom::new(f64::NAN, 2.0),
            Err(BloomError::Strength(_))
        ));
        assert_eq!(Bloom::new(0.5, 0.0), Err(BloomError::Sigma(0.0)));
        assert_eq!(Bloom::new(0.5, 33.0), Err(BloomError::Sigma(33.0)));
        assert!(matches!(
            Bloom::new(0.5, f64::NAN),
            Err(BloomError::Sigma(_))
        ));
        let layout = FrameLayout::tight(PixelFormat::Rgba8, 4, 4).expect("layout");
        let mut rgba8 = FrameBuffer::new(layout);
        assert!(matches!(
            Bloom::new(0.5, 1.0).expect("bloom").apply(&mut rgba8, 1),
            Err(FrameError::FormatMismatch { .. })
        ));
    }

    #[test]
    fn strength_zero_returns_the_frame_unchanged() {
        let original = frame(37, 23, speckle);
        let mut bloomed = original.clone();
        Bloom::new(0.0, 4.0)
            .expect("bloom")
            .apply(&mut bloomed, 3)
            .expect("blooms");
        assert_eq!(bloomed.as_bytes(), original.as_bytes());
    }

    #[test]
    fn a_bloom_spreads_a_dot_conserving_its_light() {
        // One opaque white pixel on black: the centre keeps 1 − s plus its
        // share of the blur, the light reaches exactly the blur's radius,
        // and the frame's total light is the dot's.
        let bloom = Bloom::new(0.25, 2.0).expect("bloom");
        let radius = bloom.radius() as u32;
        let mut bloomed = frame(41, 41, |x, y| {
            if (x, y) == (20, 20) {
                [1.0; 4]
            } else {
                [0.0; 4]
            }
        });
        bloom.apply(&mut bloomed, 2).expect("blooms");
        let centre = bloom.taps()[bloom.radius()];
        let expected = 0.75 + 0.25 * centre * centre;
        assert!((value(&bloomed, 20, 20, 0) - expected).abs() < 1e-3);
        assert!(value(&bloomed, 20 + radius, 20, 0) > 0.0);
        assert_eq!(value(&bloomed, 21 + radius, 20, 0), 0.0);
        assert_eq!(value(&bloomed, 20, 21 + radius, 3), 0.0);
        // Centred on the dot: mirror images across it, bit for bit.
        for d in 1..=radius + 1 {
            for c in 0..4 {
                let [right, left] = [20 + d, 20 - d].map(|x| value(&bloomed, x, 20, c).to_bits());
                let [below, above] = [20 + d, 20 - d].map(|y| value(&bloomed, 20, y, c).to_bits());
                assert_eq!((right, below), (left, above), "{d} {c}");
            }
        }
        let mut total = 0.0_f64;
        for y in 0..41 {
            for x in 0..41 {
                total += f64::from(value(&bloomed, x, y, 1));
            }
        }
        assert!((total - 1.0).abs() < 2e-3, "{total}");
    }

    #[test]
    fn an_edge_keeps_the_light_past_it_and_does_not_wrap() {
        // A dot on the left edge: the taps past the edge read the edge, so
        // their light stays on it, and none reaches the far side.
        let bloom = Bloom::new(0.5, 2.0).expect("bloom");
        let radius = bloom.radius();
        let mut bloomed = frame(41, 41, |x, y| {
            if (x, y) == (0, 20) {
                [1.0; 4]
            } else {
                [0.0; 4]
            }
        });
        bloom.apply(&mut bloomed, 2).expect("blooms");
        let off_and_on: f32 = bloom.taps()[..=radius].iter().sum();
        let expected = 0.5 + 0.5 * bloom.taps()[radius] * off_and_on;
        assert!((value(&bloomed, 0, 20, 0) - expected).abs() < 1e-3);
        assert!(value(&bloomed, radius as u32, 20, 0) > 0.0);
        for x in radius as u32 + 1..41 {
            for c in 0..4 {
                assert_eq!(value(&bloomed, x, 20, c), 0.0, "{x} {c}");
            }
        }
    }

    #[test]
    fn a_bloom_stays_within_its_window_and_its_alpha() {
        // A convex mix: no channel above the brightest (or below the
        // darkest) of that channel within the blur's reach, colour within
        // alpha; each to the f16 rounding of the result.
        let bloom = Bloom::new(0.6, 1.5).expect("bloom");
        let radius = bloom.radius() as i64;
        let original = frame(29, 31, speckle);
        let mut bloomed = original.clone();
        bloom.apply(&mut bloomed, 4).expect("blooms");
        let ulp = |v: f32| v.abs() * 2.0_f32.powi(-10) + 1e-7;
        for y in 0..31_i64 {
            for x in 0..29_i64 {
                for c in 0..4 {
                    let (mut low, mut high) = (f32::MAX, f32::MIN);
                    for dy in -radius..=radius {
                        for dx in -radius..=radius {
                            let v = value(
                                &original,
                                (x + dx).clamp(0, 28) as u32,
                                (y + dy).clamp(0, 30) as u32,
                                c,
                            );
                            low = low.min(v);
                            high = high.max(v);
                        }
                    }
                    let v = value(&bloomed, x as u32, y as u32, c);
                    assert!(
                        v <= high + ulp(high) && v >= low - ulp(low),
                        "({x}, {y}) {c}"
                    );
                }
                let alpha = value(&bloomed, x as u32, y as u32, 3);
                for c in 0..3 {
                    assert!(value(&bloomed, x as u32, y as u32, c) <= alpha + ulp(alpha));
                }
            }
        }
    }

    #[test]
    fn a_thin_dark_boundary_keeps_its_contrast() {
        // A one-pixel black column between two bright fields keeps at least
        // 1 − 2s of its contrast: here (s = 0.15) 70%.
        let strength = 0.15;
        let bloom = Bloom::new(strength, 3.0).expect("bloom");
        let mut bloomed = frame(40, 9, |x, _| {
            if x == 20 {
                [0.0, 0.0, 0.0, 1.0]
            } else {
                [0.8, 0.8, 0.8, 1.0]
            }
        });
        bloom.apply(&mut bloomed, 1).expect("blooms");
        let gap = value(&bloomed, 20, 4, 0);
        let side = value(&bloomed, 19, 4, 0).min(value(&bloomed, 21, 4, 0));
        assert!(
            side - gap >= (1.0 - 2.0 * strength as f32) * 0.8,
            "{side} {gap}"
        );
        assert!(gap > 0.0, "the bloom reaches the gap");
    }

    #[test]
    fn the_bits_do_not_depend_on_the_thread_count() {
        let bloom = Bloom::new(0.3, 2.5).expect("bloom");
        let original = frame(53, 47, speckle);
        let mut one = original.clone();
        bloom.apply(&mut one, 1).expect("blooms");
        for threads in [2, 3, 8] {
            let mut many = original.clone();
            bloom.apply(&mut many, threads).expect("blooms");
            assert_eq!(many.as_bytes(), one.as_bytes(), "{threads} threads");
        }
        assert_ne!(one.as_bytes(), original.as_bytes());
    }
}
