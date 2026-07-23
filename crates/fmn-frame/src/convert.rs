//! Format conversion kernels (§14.1, §14.3, §17.3 hot list: "color
//! transfer functions").
//!
//! Three kernels and a swizzle:
//!
//! - [`rgba16f_to_rgba8`] — linear-light RGBA16F → sRGB RGBA8, the
//!   certified canonical-output conversion: pure table lookup in fixed
//!   row-major order, bit-exact on every platform (W5CERT's rules).
//! - [`rgba_to_nv12`] / [`rgba_to_p010`] — RGB → BT.709 Y′CbCr 4:2:0
//!   in Q16.16 integer fixed point with defined rounding; standard-mode
//!   kernels (they feed ffmpeg, whose products are uncertified by
//!   construction) but deterministic everywhere regardless.
//! - [`swap_rb8`] — RGBA8 ⇄ BGRA8 for compatibility sinks.
//!
//! Every kernel honors per-plane strides (padding bytes are never
//! touched), allocates nothing, and reads/writes in output orientation
//! only (D-23: no vflip exists anywhere in the system).
//!
//! # The fixed-point contract
//!
//! Coefficients are BT.709 (Kr = 0.2126, Kb = 0.0722) scaled by 2¹⁶ and
//! rounded to the nearest integer, with each chroma row nudged by at
//! most one ulp so it sums to exactly zero — neutral gray maps to the
//! exact chroma midpoint by construction. Quantization is
//! `(sum + 2¹⁵) >> 16` (arithmetic shift: floor), i.e. round half up on
//! the whole number line, then offset and clamp. The 10-bit path reuses
//! the same Q16.16 sums with a 14-bit shift, so 8- and 10-bit outputs
//! quantize one common intermediate.

use crate::FrameError;
use crate::buffer::FrameBuffer;
use crate::format::{ChromaSiting, ColorRange, PixelFormat};
use crate::transfer::tables;

/// One BT.709 RGB → Y′CbCr coefficient set, Q16.16.
struct Coef {
    y: [i64; 3],
    cb: [i64; 3],
    cr: [i64; 3],
    /// 8-bit Y′ offset (16 limited, 0 full). Chroma offset is 128.
    y_off: i64,
}

/// Full-range RGB → limited-range Y′CbCr (BT.709). Y rows scaled by
/// 219/255, chroma by 224/255. `cr[2]` nudged −2639 → −2640 for an
/// exact-zero row sum.
const BT709_LIMITED: Coef = Coef {
    y: [11966, 40254, 4064],
    cb: [-6596, -22189, 28785],
    cr: [28785, -26145, -2640],
    y_off: 16,
};

/// Full-range RGB → full-range Y′CbCr (BT.709). Rows sum to 65536 (Y)
/// and 0 (chroma) with natural rounding — no nudge needed.
const BT709_FULL: Coef = Coef {
    y: [13933, 46871, 4732],
    cb: [-7509, -25259, 32768],
    cr: [32768, -29763, -3005],
    y_off: 0,
};

impl Coef {
    const fn for_range(range: ColorRange) -> &'static Self {
        match range {
            ColorRange::Limited => &BT709_LIMITED,
            ColorRange::Full => &BT709_FULL,
        }
    }
}

fn dot(c: &[i64; 3], r: u8, g: u8, b: u8) -> i64 {
    c[0] * i64::from(r) + c[1] * i64::from(g) + c[2] * i64::from(b)
}

/// Q16.16 → 8-bit code: round half up, offset, clamp.
fn quant8(sum: i64, offset: i64) -> u8 {
    (((sum + (1 << 15)) >> 16) + offset).clamp(0, 255) as u8
}

/// Q16.16 → 10-bit code: same intermediate, 14-bit shift, 4× offsets.
fn quant10(sum: i64, offset: i64) -> u16 {
    (((sum + (1 << 13)) >> 14) + offset * 4).clamp(0, 1023) as u16
}

/// The (r, g, b, a) byte offsets of an interleaved 8-bit RGBA-family
/// pixel, or `None` if `format` is not one.
const fn rgba8_offsets(format: PixelFormat) -> Option<[usize; 4]> {
    match format {
        PixelFormat::Rgba8 => Some([0, 1, 2, 3]),
        PixelFormat::Bgra8 => Some([2, 1, 0, 3]),
        _ => None,
    }
}

fn check_dims(src: &FrameBuffer, dst: &FrameBuffer) -> Result<(), FrameError> {
    if src.layout().width() != dst.layout().width()
        || src.layout().height() != dst.layout().height()
    {
        return Err(FrameError::DimensionMismatch);
    }
    Ok(())
}

/// Linear-light RGBA16F → sRGB-encoded RGBA8, the certified
/// canonical-output kernel.
///
/// Color channels pass through the sRGB OETF, alpha stays linear
/// (coverage is never gamma-encoded); both are 65536-entry table
/// lookups with defined bytes for every input bit pattern (negative,
/// infinite, and NaN samples included). Fixed traversal order,
/// no arithmetic on the hot path — bit-exact by construction.
pub fn rgba16f_to_rgba8(src: &FrameBuffer, dst: &mut FrameBuffer) -> Result<(), FrameError> {
    if src.layout().format() != PixelFormat::Rgba16F {
        return Err(FrameError::FormatMismatch {
            expected: "Rgba16F source",
            got: src.layout().format(),
        });
    }
    if dst.layout().format() != PixelFormat::Rgba8 {
        return Err(FrameError::FormatMismatch {
            expected: "Rgba8 destination",
            got: dst.layout().format(),
        });
    }
    check_dims(src, dst)?;

    let width = src.layout().width() as usize;
    let height = src.layout().height() as usize;
    let src_stride = src.layout().stride(0);
    let dst_stride = dst.layout().stride(0);
    let t = tables();

    let src_plane = src.plane(0);
    let dst_plane = dst.plane_mut(0);
    for y in 0..height {
        let s_row = &src_plane[y * src_stride..y * src_stride + width * 8];
        let d_row = &mut dst_plane[y * dst_stride..y * dst_stride + width * 4];
        for x in 0..width {
            let s = &s_row[x * 8..x * 8 + 8];
            let d = &mut d_row[x * 4..x * 4 + 4];
            for ch in 0..3 {
                let bits = u16::from_le_bytes([s[ch * 2], s[ch * 2 + 1]]);
                d[ch] = t.srgb8_from_f16(bits);
            }
            let a_bits = u16::from_le_bytes([s[6], s[7]]);
            d[3] = t.linear8_from_f16(a_bits);
        }
    }
    Ok(())
}

/// Validate a 4:2:0 conversion's formats and dimensions; returns the
/// source's (r, g, b, a) channel offsets.
fn check_yuv420_inputs(
    src: &FrameBuffer,
    dst: &FrameBuffer,
    dst_format: PixelFormat,
    expected: &'static str,
) -> Result<[usize; 4], FrameError> {
    let offsets = rgba8_offsets(src.layout().format()).ok_or(FrameError::FormatMismatch {
        expected: "Rgba8 or Bgra8 source",
        got: src.layout().format(),
    })?;
    if dst.layout().format() != dst_format {
        return Err(FrameError::FormatMismatch {
            expected,
            got: dst.layout().format(),
        });
    }
    check_dims(src, dst)?;
    Ok(offsets)
}

/// Average a 2×2 quad of Q16.16 chroma sums per the siting rule, with
/// round-half-up division.
fn site_average(siting: ChromaSiting, s00: i64, s01: i64, s10: i64, s11: i64) -> i64 {
    match siting {
        // Horizontally co-sited with the left luma column, vertically
        // interstitial: average the left column's two rows.
        ChromaSiting::Left => (s00 + s10 + 1) >> 1,
        // Interstitial both ways: box-average the quad.
        ChromaSiting::Center => (s00 + s01 + s10 + s11 + 2) >> 2,
    }
}

/// RGBA8/BGRA8 → NV12 (BT.709), with explicit range and chroma-siting
/// semantics. Standard-mode (feeds the ffmpeg boundary), deterministic
/// everywhere. Requires even dimensions (enforced by the NV12 layout).
pub fn rgba_to_nv12(
    src: &FrameBuffer,
    dst: &mut FrameBuffer,
    range: ColorRange,
    siting: ChromaSiting,
) -> Result<(), FrameError> {
    let [ro, go, bo, _] = check_yuv420_inputs(src, dst, PixelFormat::Nv12, "Nv12 destination")?;
    let coef = Coef::for_range(range);

    let width = src.layout().width() as usize;
    let height = src.layout().height() as usize;
    let src_stride = src.layout().stride(0);
    let y_stride = dst.layout().stride(0);
    let c_stride = dst.layout().stride(1);
    let c_offset = dst.layout().plane_offset(1);
    let y_offset = dst.layout().plane_offset(0);

    let src_plane = src.plane(0);
    let dst_bytes = dst.as_bytes_mut();

    for y in (0..height).step_by(2) {
        for x in (0..width).step_by(2) {
            let mut y_codes = [[0u8; 2]; 2];
            let mut c_sums = [[0i64; 2]; 4]; // [quad index][cb, cr]
            for dy in 0..2 {
                let row = &src_plane[(y + dy) * src_stride..];
                for dx in 0..2 {
                    let p = &row[(x + dx) * 4..(x + dx) * 4 + 4];
                    let (r, g, b) = (p[ro], p[go], p[bo]);
                    y_codes[dy][dx] = quant8(dot(&coef.y, r, g, b), coef.y_off);
                    c_sums[dy * 2 + dx] = [dot(&coef.cb, r, g, b), dot(&coef.cr, r, g, b)];
                }
            }
            for (dy, codes) in y_codes.iter().enumerate() {
                let base = y_offset + (y + dy) * y_stride + x;
                dst_bytes[base] = codes[0];
                dst_bytes[base + 1] = codes[1];
            }
            let c_base = c_offset + (y / 2) * c_stride + x;
            for (ch, slot) in [(0usize, c_base), (1usize, c_base + 1)] {
                let avg = site_average(
                    siting,
                    c_sums[0][ch],
                    c_sums[1][ch],
                    c_sums[2][ch],
                    c_sums[3][ch],
                );
                dst_bytes[slot] = quant8(avg, 128);
            }
        }
    }
    Ok(())
}

/// RGBA8/BGRA8 → P010 (BT.709, limited range only — full-range 10-bit
/// video is not a negotiable sink format; requesting it is a typed
/// refusal, not a silent substitution). Samples are 10-bit codes
/// MSB-aligned in little-endian `u16`s.
pub fn rgba_to_p010(
    src: &FrameBuffer,
    dst: &mut FrameBuffer,
    range: ColorRange,
    siting: ChromaSiting,
) -> Result<(), FrameError> {
    if range != ColorRange::Limited {
        return Err(FrameError::UnsupportedConversion(
            "P010 output is limited-range only",
        ));
    }
    let [ro, go, bo, _] = check_yuv420_inputs(src, dst, PixelFormat::P010, "P010 destination")?;
    let coef = &BT709_LIMITED;

    let width = src.layout().width() as usize;
    let height = src.layout().height() as usize;
    let src_stride = src.layout().stride(0);
    let y_stride = dst.layout().stride(0);
    let c_stride = dst.layout().stride(1);
    let c_offset = dst.layout().plane_offset(1);
    let y_offset = dst.layout().plane_offset(0);

    let src_plane = src.plane(0);
    let dst_bytes = dst.as_bytes_mut();

    let put16 = |bytes: &mut [u8], at: usize, code: u16| {
        let msb = code << 6; // MSB-aligned, low 6 bits zero
        bytes[at] = (msb & 0xff) as u8;
        bytes[at + 1] = (msb >> 8) as u8;
    };

    for y in (0..height).step_by(2) {
        for x in (0..width).step_by(2) {
            let mut y_codes = [[0u16; 2]; 2];
            let mut c_sums = [[0i64; 2]; 4];
            for dy in 0..2 {
                let row = &src_plane[(y + dy) * src_stride..];
                for dx in 0..2 {
                    let p = &row[(x + dx) * 4..(x + dx) * 4 + 4];
                    let (r, g, b) = (p[ro], p[go], p[bo]);
                    y_codes[dy][dx] = quant10(dot(&coef.y, r, g, b), coef.y_off);
                    c_sums[dy * 2 + dx] = [dot(&coef.cb, r, g, b), dot(&coef.cr, r, g, b)];
                }
            }
            for (dy, codes) in y_codes.iter().enumerate() {
                let base = y_offset + (y + dy) * y_stride + x * 2;
                put16(dst_bytes, base, codes[0]);
                put16(dst_bytes, base + 2, codes[1]);
            }
            let c_base = c_offset + (y / 2) * c_stride + x * 2;
            for (ch, slot) in [(0usize, c_base), (1usize, c_base + 2)] {
                let avg = site_average(
                    siting,
                    c_sums[0][ch],
                    c_sums[1][ch],
                    c_sums[2][ch],
                    c_sums[3][ch],
                );
                put16(dst_bytes, slot, quant10(avg, 128));
            }
        }
    }
    Ok(())
}

/// RGBA8 ⇄ BGRA8 channel swizzle (either direction — the kernel swaps
/// the R and B lanes of whichever 8-bit four-channel formats it gets).
pub fn swap_rb8(src: &FrameBuffer, dst: &mut FrameBuffer) -> Result<(), FrameError> {
    let src_fmt = src.layout().format();
    let dst_fmt = dst.layout().format();
    if rgba8_offsets(src_fmt).is_none() {
        return Err(FrameError::FormatMismatch {
            expected: "Rgba8 or Bgra8 source",
            got: src_fmt,
        });
    }
    if rgba8_offsets(dst_fmt).is_none() || dst_fmt == src_fmt {
        return Err(FrameError::FormatMismatch {
            expected: "the opposite 8-bit RGBA-family format",
            got: dst_fmt,
        });
    }
    check_dims(src, dst)?;

    let width = src.layout().width() as usize;
    let height = src.layout().height() as usize;
    let src_stride = src.layout().stride(0);
    let dst_stride = dst.layout().stride(0);
    let src_plane = src.plane(0);
    let dst_plane = dst.plane_mut(0);
    for y in 0..height {
        let s_row = &src_plane[y * src_stride..y * src_stride + width * 4];
        let d_row = &mut dst_plane[y * dst_stride..y * dst_stride + width * 4];
        for x in 0..width {
            let s = &s_row[x * 4..x * 4 + 4];
            let d = &mut d_row[x * 4..x * 4 + 4];
            d[0] = s[2];
            d[1] = s[1];
            d[2] = s[0];
            d[3] = s[3];
        }
    }
    Ok(())
}
