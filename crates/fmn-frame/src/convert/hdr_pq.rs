//! Explicit display-referred BT.2020/PQ output (§14.3).
//!
//! The working framebuffer uses linear sRGB/BT.709 primaries. Convert those
//! primaries in linear light, assign an absolute luminance, then apply the PQ
//! inverse EOTF. A P010 container or a metadata tag alone is not this transform.
//!
//! Standards: ITU-R BT.2100-3 tables 4 (PQ), 6 (non-constant-luminance Y′CbCr),
//! 9 (integer coding), and 10 (reference-white normalization):
//! <https://www.itu.int/rec/R-REC-BT.2100-3-202502-I/en>.
//! The D65 primary transform is the product of the rational `lin_sRGB_to_XYZ`
//! and `XYZ_to_lin_2020` matrices in W3C CSS Color 4:
//! <https://www.w3.org/TR/css-color-4/#color-conversion-code>.

use super::{Coef, check_dims, check_rgba16f_source, linear_yuv};
use crate::half::f16_to_f64;
use crate::{ChromaSiting, ColorRange, FrameBuffer, FrameError, PixelFormat};

/// Explicit luminance normalization for BT.2020/PQ video output.
///
/// Working-space linear RGB `1.0` represents `reference_white_nits` cd/m².
/// Values above one retain their highlight headroom. After conversion to
/// BT.2020 primaries, each absolute-light component is clipped to `peak_nits`.
/// This is a hard content limit, not a tone mapper or a claim about a mastering
/// display. No mastering-display or content-light metadata is fabricated.
///
/// The default is a 203 cd/m² reference white and a 1,000 cd/m² content limit.
/// Construction admits only `0 < reference_white_nits <= peak_nits <= 10,000`,
/// with both values finite. The immutable value is cheap to copy and does not
/// allocate or initialize any of the certified SDR transfer tables.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct HdrPqConfig {
    reference_white_nits: f64,
    peak_nits: f64,
}

// The fields are private and every construction path guarantees finite,
// positive values. NaN cannot break reflexivity, so Eq is sound here.
impl Eq for HdrPqConfig {}

impl Default for HdrPqConfig {
    fn default() -> Self {
        Self {
            reference_white_nits: 203.0,
            peak_nits: 1_000.0,
        }
    }
}

impl HdrPqConfig {
    /// Validate absolute reference-white and clipping luminances in cd/m².
    /// Invalid settings are refused, never clamped or silently substituted.
    pub fn try_new(reference_white_nits: f64, peak_nits: f64) -> Result<Self, FrameError> {
        if !reference_white_nits.is_finite()
            || reference_white_nits <= 0.0
            || reference_white_nits > 10_000.0
        {
            return Err(FrameError::UnsupportedConversion(
                "HDR PQ reference white must be finite and in 0 < nits <= 10000",
            ));
        }
        if !peak_nits.is_finite() || peak_nits < reference_white_nits || peak_nits > 10_000.0 {
            return Err(FrameError::UnsupportedConversion(
                "HDR PQ peak must be finite and in reference_white_nits..=10000",
            ));
        }
        Ok(Self {
            reference_white_nits,
            peak_nits,
        })
    }

    /// Absolute light represented by working-space linear RGB `1.0`, in cd/m².
    #[must_use]
    pub const fn reference_white_nits(self) -> f64 {
        self.reference_white_nits
    }

    /// Hard clipping limit for BT.2020 light components, in cd/m².
    #[must_use]
    pub const fn peak_nits(self) -> f64 {
        self.peak_nits
    }
}

// Linear sRGB (D65) -> linear BT.2020 (D65). These are the correctly rounded
// f64 entries of XYZ_to_lin_2020 * lin_sRGB_to_XYZ from the rational matrices
// cited above. The exact rational rows sum to one; no white adaptation occurs.
const SRGB_TO_BT2020: [[f64; 3]; 3] = [
    [
        0.627_403_895_934_699,
        0.329_283_038_377_883_7,
        0.043_313_065_687_417_225,
    ],
    [
        0.069_097_289_358_232_08,
        0.919_540_395_075_458_7,
        0.011_362_315_566_309_178,
    ],
    [
        0.016_391_438_875_150_28,
        0.088_013_307_877_225_75,
        0.895_595_253_247_624,
    ],
];

// BT.2100 table 6: Kr=.2627, Kg=.6780, Kb=.0593. Scale luma by 219/255
// and chroma by 224/255, then round each coefficient to Q16.16. Both chroma
// rows sum to exactly zero without adjustment. The shared 16-bit-RGB kernel
// applies the 10-bit limited-range offsets and round-half-up quantization.
const BT2020_LIMITED: Coef = Coef {
    y: [14_786, 38_160, 3_338],
    cb: [-8_038, -20_746, 28_784],
    cr: [28_784, -26_469, -2_315],
    y_off: 16,
};

fn pq_encode_nits(nits: f64) -> f64 {
    // Exact binary fractions from BT.2100 table 4 / SMPTE ST 2084.
    const M1: f64 = 2610.0 / 16_384.0;
    const M2: f64 = 2523.0 / 32.0;
    const C1: f64 = 3424.0 / 4096.0;
    const C2: f64 = 2413.0 / 128.0;
    const C3: f64 = 2392.0 / 128.0;
    let light = fmn_dmath::pow(nits / 10_000.0, M1);
    fmn_dmath::pow((C1 + C2 * light) / (1.0 + C3 * light), M2)
}

fn encoded_pixel(pixel: &[u8], config: HdrPqConfig) -> [i64; 3] {
    let rgb = [0, 2, 4].map(|at| {
        let value = f16_to_f64(u16::from_le_bytes([pixel[at], pixel[at + 1]]));
        if value.is_nan() || value < 0.0 {
            0.0
        } else {
            value
        }
    });
    let quantize = |linear: f64| {
        let nits = (linear * config.reference_white_nits).clamp(0.0, config.peak_nits);
        (pq_encode_nits(nits) * 65535.0 + 0.5).floor() as i64
    };
    if rgb[0] == rgb[1] && rgb[1] == rgb[2] {
        // Preserve achromatic inputs exactly, including at a quantization
        // boundary: matrix-rounding noise must never introduce chroma.
        return [quantize(rgb[0]); 3];
    }
    SRGB_TO_BT2020.map(|row| {
        // All coefficients are positive. A positive-infinite input therefore
        // reaches the explicit clipping limit without any inf*0 / NaN cases.
        quantize(row[0] * rgb[0] + row[1] * rgb[1] + row[2] * rgb[2])
    })
}

/// Linear-light sRGB-primary RGBA16F → limited-range BT.2020/PQ P010.
///
/// The input must already be composited; alpha is ignored. Values above one
/// survive until the explicit luminance limit in `config`. Negative and NaN
/// source channels become zero. Positive infinity saturates the affected
/// BT.2020 components at that limit. The primary transform precedes the PQ
/// inverse EOTF; PQ RGB retains 16-bit precision until final 10-bit Y′CbCr
/// quantization. Neutral chroma is exactly 512. Samples are MSB-aligned,
/// little-endian u16s with their low six bits zero.
///
/// This is an explicit standard-mode output transform. It never changes the
/// certified sRGB conversion or the existing SDR P010 conversion. Sinks must
/// signal BT.2020 primaries, SMPTE ST 2084 transfer, BT.2020 non-constant-
/// luminance matrix, and the supplied chroma siting. This function does not
/// provide mastering-display metadata or certify an HDR10 deliverable.
///
/// Full range, mismatched formats and mismatched dimensions fail before any
/// destination write. Source and destination row padding are untouched. The
/// serial kernel allocates no scratch storage; it reuses the existing P010
/// row-pair kernel and its exact repeated-quad memoization.
pub fn rgba16f_to_p010_hdr_pq(
    src: &FrameBuffer,
    dst: &mut FrameBuffer,
    range: ColorRange,
    siting: ChromaSiting,
    config: HdrPqConfig,
) -> Result<(), FrameError> {
    rgba16f_to_p010_hdr_pq_threaded(src, dst, range, siting, config, 1)
}

/// [`rgba16f_to_p010_hdr_pq`] with disjoint row-pair bands on scoped workers.
///
/// Output bytes are identical for every thread count. Small frames, zero or
/// one requested thread, and wasm32 execute serially. Refused worker spawns
/// execute their bands on the caller; no output band can be skipped.
pub fn rgba16f_to_p010_hdr_pq_threaded(
    src: &FrameBuffer,
    dst: &mut FrameBuffer,
    range: ColorRange,
    siting: ChromaSiting,
    config: HdrPqConfig,
    threads: usize,
) -> Result<(), FrameError> {
    if range != ColorRange::Limited {
        return Err(FrameError::UnsupportedConversion(
            "HDR PQ P010 output is limited-range only",
        ));
    }
    check_rgba16f_source(src)?;
    if dst.layout().format() != PixelFormat::P010 {
        return Err(FrameError::FormatMismatch {
            expected: "P010 destination",
            got: dst.layout().format(),
        });
    }
    check_dims(src, dst)?;
    linear_yuv::convert::<true>(src, dst, &BT2020_LIMITED, siting, threads, |pixel| {
        encoded_pixel(pixel, config)
    });
    Ok(())
}
