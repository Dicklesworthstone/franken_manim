//! Native movie grading after the single output transfer (§14.3).
//!
//! The input is already sRGB-encoded RGB, or its BT.709 Y′CbCr conversion.
//! Gamma maps normalized encoded luma `Y` to `Y^(1/gamma)`; saturation
//! scales centered chroma. This is a creative adjustment, not another
//! transfer function. Callers apply it to movie frames only, once, after
//! conversion and before publication. Still images and canonical PNG output
//! retain their ordinary transfer without this adjustment.
//!
//! RGB uses BT.709 weights `(0.2126, 0.7152, 0.0722)`. Scaling both chroma
//! components is equivalent to `channel′ = graded_Y + saturation*(channel-Y)`.
//! RGB is clamped to `[0,1]` before round-half-up byte quantization. Alpha is
//! unchanged; RGB is graded even when alpha is zero.
//!
//! NV12 and P010 here mean the native wire's **limited range**: Y′ codes
//! `16..=235` / `64..=940`, chroma `16..=240` / `64..=960`, with midpoint
//! `128` / `512`. Normalize luma against those legal endpoints, apply gamma,
//! then map back. Chroma scales about its midpoint. Nonidentity grades clamp
//! input and output to the legal ranges and round half up. P010 retains all
//! ten bits and writes canonical MSB-aligned little-endian samples. No range
//! is inferred from pixels; full-range YUV is not this API's input contract.
//!
//! This defined native rule is not byte identity with the Reference's old
//! ffmpeg `eq` filter, which implicitly negotiated 8-bit YUV and applied its
//! gamma curve to unnormalized code values. No ffmpeg filter is required.

use std::sync::Arc;

use crate::transfer::quantize8;
use crate::{FrameBuffer, FrameError, PixelFormat};

struct GradeTables {
    luma8: [u8; 256],
    chroma8: [u8; 256],
    luma10: [u16; 1024],
    chroma10: [u16; 1024],
}

/// An immutable, reusable movie color adjustment.
///
/// Construction validates the complete request before allocating its small
/// Y′CbCr lookup tables. Clones share those tables; [`Self::apply`] needs no
/// scratch allocation and never changes frame storage, orientation, alpha,
/// or row padding. The identity grade allocates no tables and traverses no
/// pixels, preserving every byte, including noncanonical YUV sample bits.
#[derive(Clone)]
pub struct VideoColorGrade {
    gamma: f64,
    saturation: f64,
    tables: Option<Arc<GradeTables>>,
}

impl std::fmt::Debug for VideoColorGrade {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("VideoColorGrade")
            .field("gamma", &self.gamma)
            .field("saturation", &self.saturation)
            .finish()
    }
}

impl Default for VideoColorGrade {
    fn default() -> Self {
        Self {
            gamma: 1.0,
            saturation: 1.0,
            tables: None,
        }
    }
}

impl VideoColorGrade {
    /// Prepare finite `gamma` in `0.1..=10` and `saturation` in `0..=3`.
    ///
    /// Gamma above one brightens luma; zero saturation removes chroma.
    /// Exactly `(1,1)` is an allocation-free identity. Validation never
    /// clamps a caller's settings or silently substitutes another grade.
    pub fn new(gamma: f64, saturation: f64) -> Result<Self, FrameError> {
        if !gamma.is_finite() || !(0.1..=10.0).contains(&gamma) {
            return Err(FrameError::InvalidVideoGamma);
        }
        if !saturation.is_finite() || !(0.0..=3.0).contains(&saturation) {
            return Err(FrameError::InvalidVideoSaturation);
        }
        if gamma == 1.0 && saturation == 1.0 {
            return Ok(Self::default());
        }
        let inverse_gamma = 1.0 / gamma;
        let tables = GradeTables {
            luma8: std::array::from_fn(|code| luma_code(code, 16, 235, inverse_gamma) as u8),
            chroma8: std::array::from_fn(|code| chroma_code(code, 16, 240, 128, saturation) as u8),
            luma10: std::array::from_fn(|code| luma_code(code, 64, 940, inverse_gamma)),
            chroma10: std::array::from_fn(|code| chroma_code(code, 64, 960, 512, saturation)),
        };
        Ok(Self {
            gamma,
            saturation,
            tables: Some(Arc::new(tables)),
        })
    }

    /// Whether applying this grade performs no pixel work or mutation.
    #[must_use]
    pub const fn is_identity(&self) -> bool {
        self.tables.is_none()
    }

    /// The validated gamma setting, for output receipts.
    #[must_use]
    pub const fn gamma(&self) -> f64 {
        self.gamma
    }

    /// The validated saturation setting, for output receipts.
    #[must_use]
    pub const fn saturation(&self) -> f64 {
        self.saturation
    }

    /// Grade encoded RGBA8/BGRA8 or limited-range NV12/P010 in place.
    ///
    /// Every format check completes before any frame byte is written.
    /// Linear-light RGBA16F is a typed refusal, including for an identity
    /// grade: it must pass through the ordinary transfer/conversion first.
    /// The validated [`crate::FrameLayout`] owns all dimension/stride checks.
    pub fn apply(&self, frame: &mut FrameBuffer) -> Result<(), FrameError> {
        let format = frame.layout().format();
        if format == PixelFormat::Rgba16F {
            return Err(FrameError::UnsupportedConversion(
                "video color grading requires encoded RGBA8/BGRA8 or limited-range NV12/P010",
            ));
        }
        let Some(tables) = self.tables.as_deref() else {
            return Ok(());
        };
        match format {
            PixelFormat::Rgba8 | PixelFormat::Bgra8 => self.apply_rgb(frame, format),
            PixelFormat::Nv12 => {
                let width = frame.layout().width() as usize;
                for (plane, table) in [&tables.luma8, &tables.chroma8].into_iter().enumerate() {
                    let stride = frame.layout().stride(plane);
                    for row in frame.plane_mut(plane).chunks_exact_mut(stride) {
                        for sample in &mut row[..width] {
                            *sample = table[usize::from(*sample)];
                        }
                    }
                }
            }
            PixelFormat::P010 => {
                let row_bytes = frame.layout().width() as usize * 2;
                for (plane, table) in [&tables.luma10, &tables.chroma10].into_iter().enumerate() {
                    let stride = frame.layout().stride(plane);
                    for row in frame.plane_mut(plane).chunks_exact_mut(stride) {
                        for sample in row[..row_bytes].as_chunks_mut::<2>().0 {
                            let code = u16::from_le_bytes(*sample) >> 6;
                            *sample = (table[usize::from(code)] << 6).to_le_bytes();
                        }
                    }
                }
            }
            // The format was refused before accessing mutable frame storage.
            PixelFormat::Rgba16F => unreachable!(),
        }
        Ok(())
    }

    fn apply_rgb(&self, frame: &mut FrameBuffer, format: PixelFormat) {
        let [red, blue] = if format == PixelFormat::Rgba8 {
            [0, 2]
        } else {
            [2, 0]
        };
        let row_bytes = frame.layout().width() as usize * 4;
        let stride = frame.layout().stride(0);
        let inverse_gamma = 1.0 / self.gamma;
        for row in frame.plane_mut(0).chunks_exact_mut(stride) {
            for pixel in row[..row_bytes].as_chunks_mut::<4>().0 {
                // Integer weights make every neutral gray's luma exactly
                // its channel value before the one division. No platform
                // libm or repeated color transfer enters the grading path.
                let weighted = 2126 * u32::from(pixel[red])
                    + 7152 * u32::from(pixel[1])
                    + 722 * u32::from(pixel[blue]);
                let luma = f64::from(weighted) / 2_550_000.0;
                let graded = gamma_curve(luma, inverse_gamma);
                for channel in &mut pixel[..3] {
                    let encoded = f64::from(*channel) / 255.0;
                    *channel = quantize8(graded + self.saturation * (encoded - luma));
                }
            }
        }
    }
}

fn gamma_curve(luma: f64, inverse_gamma: f64) -> f64 {
    if inverse_gamma == 1.0 || luma == 0.0 || luma == 1.0 {
        luma
    } else {
        fmn_dmath::pow(luma, inverse_gamma)
    }
}

fn luma_code(code: usize, min: usize, max: usize, inverse_gamma: f64) -> u16 {
    let code = code.clamp(min, max);
    if inverse_gamma == 1.0 {
        return code as u16;
    }
    let normalized = (code - min) as f64 / (max - min) as f64;
    let graded = gamma_curve(normalized, inverse_gamma);
    ((min as f64 + (max - min) as f64 * graded + 0.5).floor() as u16).clamp(min as u16, max as u16)
}

fn chroma_code(code: usize, min: usize, max: usize, midpoint: usize, saturation: f64) -> u16 {
    let centered = code.clamp(min, max) as f64 - midpoint as f64;
    let graded = midpoint as f64 + saturation * centered;
    (graded.clamp(min as f64, max as f64) + 0.5).floor() as u16
}
