//! Direct binary16 output conversion, without a frame-sized RGBA8 intermediate.

use std::sync::OnceLock;

use super::{Coef, check_dims, check_rgba16f_source, quant8, site_average};
use crate::half::f16_to_f64;
use crate::transfer::{srgb_encode, tables};
use crate::{ChromaSiting, ColorRange, FrameBuffer, FrameError, PixelFormat};

fn check_output(
    src: &FrameBuffer,
    dst: &FrameBuffer,
    format: PixelFormat,
    expected: &'static str,
) -> Result<(), FrameError> {
    check_rgba16f_source(src)?;
    if dst.layout().format() != format {
        return Err(FrameError::FormatMismatch {
            expected,
            got: dst.layout().format(),
        });
    }
    check_dims(src, dst)
}

fn channel(pixel: &[u8], at: usize) -> u16 {
    u16::from_le_bytes([pixel[at], pixel[at + 1]])
}

/// Linear-light RGBA16F → sRGB BGRA8 without an intermediate frame.
///
/// Equivalent byte for byte to [`super::rgba16f_to_rgba8`] followed by
/// [`super::swap_rb8`], including coverage alpha and non-finite input policy.
/// Source and destination padding are untouched. Layout refusals occur before
/// any destination write. The process-wide transfer tables initialize once;
/// subsequent conversions allocate nothing.
pub fn rgba16f_to_bgra8(src: &FrameBuffer, dst: &mut FrameBuffer) -> Result<(), FrameError> {
    check_output(src, dst, PixelFormat::Bgra8, "Bgra8 destination")?;
    let width = src.layout().width() as usize;
    let height = src.layout().height() as usize;
    let src_stride = src.layout().stride(0);
    let dst_stride = dst.layout().stride(0);
    let table = tables();
    let src_plane = src.plane(0);
    let dst_plane = dst.plane_mut(0);
    for y in 0..height {
        let source = &src_plane[y * src_stride..y * src_stride + width * 8];
        let target = &mut dst_plane[y * dst_stride..y * dst_stride + width * 4];
        let (source, _) = source.as_chunks::<8>();
        let (target, _) = target.as_chunks_mut::<4>();
        for (s, d) in source.iter().zip(target.iter_mut()) {
            *d = [
                table.srgb8_from_f16(channel(s, 4)),
                table.srgb8_from_f16(channel(s, 2)),
                table.srgb8_from_f16(channel(s, 0)),
                table.linear8_from_f16(channel(s, 6)),
            ];
        }
    }
    Ok(())
}

/// Linear-light RGBA16F → NV12, using the existing sRGB transfer and BT.709
/// matrix, with explicit range and chroma siting.
///
/// This fused conversion deliberately preserves every byte of the legacy
/// RGBA16F → RGBA8 → NV12 route. It performs that same 8-bit quantization in
/// registers, never in a frame-sized temporary allocation. Alpha is ignored
/// exactly as in [`super::rgba_to_nv12`]; callers must composite first.
/// The transfer is **sRGB**, not a BT.709 OETF or HDR transfer.
pub fn rgba16f_to_nv12(
    src: &FrameBuffer,
    dst: &mut FrameBuffer,
    range: ColorRange,
    siting: ChromaSiting,
) -> Result<(), FrameError> {
    check_output(src, dst, PixelFormat::Nv12, "Nv12 destination")?;
    let table = tables();
    convert::<false>(src, dst, Coef::for_range(range), siting, |pixel| {
        [0, 2, 4].map(|at| i64::from(table.srgb8_from_f16(channel(pixel, at))))
    });
    Ok(())
}

// Kept separate from the certified byte tables: ordinary PNG/NV12 output does
// not pay for, initialize or change its arithmetic to support a P010 request.
fn srgb16_table() -> &'static [u16; 65536] {
    static TABLE: OnceLock<Box<[u16; 65536]>> = OnceLock::new();
    TABLE
        .get_or_init(|| {
            let mut table = Box::new([0u16; 65536]);
            for bits in 0..=u16::MAX {
                let value = f16_to_f64(bits);
                let linear = if value.is_nan() {
                    0.0
                } else {
                    value.clamp(0.0, 1.0)
                };
                table[usize::from(bits)] = (srgb_encode(linear) * 65535.0 + 0.5).floor() as u16;
            }
            table
        })
        .as_ref()
}

/// Linear-light RGBA16F → limited-range P010, retaining precision beyond
/// RGBA8. Color is sRGB-encoded at 16-bit precision before applying the BT.709
/// matrix; only the final Y′/Cb/Cr samples are quantized to 10 bits.
///
/// The source is already composited linear light; alpha is ignored. Negative
/// and NaN inputs map to zero, positive infinity and values above one to one.
/// This is an SDR sRGB transfer, **not** BT.709, PQ or HLG. Sinks must describe
/// the applied transfer; a 10-bit container alone does not make a render HDR.
///
/// Coefficients, offsets and siting are shared with [`super::rgba_to_p010`].
/// The 16-bit RGB code has 257 times the scale of an 8-bit input, so rounding
/// divides each Q16.16 sum by `257 * 2^14`, with round-half-up on the signed
/// number line, followed by four-times offsets. Neutral chroma is exactly 512.
/// Output samples are MSB-aligned little-endian u16s with zero low six bits.
///
/// Uniform-color inverse-matrix error is at most 2/1023 per encoded RGB channel
/// against the clamped, transferred binary16 source; chroma subsampling adds
/// spatial error for nonuniform quads. Source half-float rounding, the encoder,
/// and any subsequent color transform are outside that bound.
///
/// Full range, mismatched formats and mismatched dimensions fail before any
/// output write. After one process-wide table initialization, conversions
/// allocate no scratch buffers or other heap storage.
pub fn rgba16f_to_p010(
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
    check_output(src, dst, PixelFormat::P010, "P010 destination")?;
    let table = srgb16_table();
    convert::<true>(src, dst, Coef::for_range(range), siting, |pixel| {
        [0, 2, 4].map(|at| i64::from(table[usize::from(channel(pixel, at))]))
    });
    Ok(())
}

fn dot(coef: &[i64; 3], rgb: [i64; 3]) -> i64 {
    coef[0] * rgb[0] + coef[1] * rgb[1] + coef[2] * rgb[2]
}

fn quant<const TEN: bool>(sum: i64, offset: i64) -> u16 {
    if TEN {
        const DIVISOR: i64 = 257 * (1 << 14);
        (((sum + DIVISOR / 2).div_euclid(DIVISOR)) + offset * 4).clamp(0, 1023) as u16
    } else {
        u16::from(quant8(sum, offset))
    }
}

fn put<const TEN: bool>(bytes: &mut [u8], at: usize, code: u16) {
    if TEN {
        bytes[at..at + 2].copy_from_slice(&(code << 6).to_le_bytes());
    } else {
        bytes[at] = code as u8;
    }
}

// Monomorphized output depth and pixel reader keep the NV12 and P010 arithmetic
// explicit. All working storage is a single 2x2 quad on the stack. The layout
// checks above and FrameLayout's even-dimension rule precede all writes.
//
// Every output code of a quad is a pure function of its four source pixels, so
// a quad whose pixels are byte-identical to the previous quad's — background,
// flat fills — reuses that quad's four luma and two chroma codes instead of
// repeating the transfer, matrix and rounding. Same bytes, less work.
fn convert<const TEN: bool>(
    src: &FrameBuffer,
    dst: &mut FrameBuffer,
    coef: &Coef,
    siting: ChromaSiting,
    rgb: impl Fn(&[u8]) -> [i64; 3],
) {
    let width = src.layout().width() as usize;
    let height = src.layout().height() as usize;
    let src_stride = src.layout().stride(0);
    let y_stride = dst.layout().stride(0);
    let c_stride = dst.layout().stride(1);
    let c_offset = dst.layout().plane_offset(1);
    let y_offset = dst.layout().plane_offset(0);
    let sample_bytes = if TEN { 2 } else { 1 };
    let source = src.plane(0);
    let target = dst.as_bytes_mut();
    let pixel_key = |at: usize| {
        u64::from_le_bytes(source[at..at + 8].try_into().expect("an 8-byte pixel"))
    };
    let mut memo: Option<([u64; 4], [u16; 4], [u16; 2])> = None;
    for y in (0..height).step_by(2) {
        for x in (0..width).step_by(2) {
            let ats = [
                y * src_stride + x * 8,
                y * src_stride + (x + 1) * 8,
                (y + 1) * src_stride + x * 8,
                (y + 1) * src_stride + (x + 1) * 8,
            ];
            let key = ats.map(pixel_key);
            let (luma, chroma_codes) = match memo {
                Some((previous, luma, chroma)) if previous == key => (luma, chroma),
                _ => {
                    let mut luma = [0u16; 4];
                    let mut chroma = [[0i64; 2]; 4];
                    for (k, &at) in ats.iter().enumerate() {
                        let pixel = rgb(&source[at..at + 8]);
                        luma[k] = quant::<TEN>(dot(&coef.y, pixel), coef.y_off);
                        chroma[k] = [dot(&coef.cb, pixel), dot(&coef.cr, pixel)];
                    }
                    let chroma_codes = [0, 1].map(|channel| {
                        let sum = site_average(
                            siting,
                            chroma[0][channel],
                            chroma[1][channel],
                            chroma[2][channel],
                            chroma[3][channel],
                        );
                        quant::<TEN>(sum, 128)
                    });
                    memo = Some((key, luma, chroma_codes));
                    (luma, chroma_codes)
                }
            };
            for dy in 0..2 {
                for dx in 0..2 {
                    let y_at = y_offset + (y + dy) * y_stride + (x + dx) * sample_bytes;
                    put::<TEN>(target, y_at, luma[dy * 2 + dx]);
                }
            }
            let c_at = c_offset + (y / 2) * c_stride + x * sample_bytes;
            for (channel, code) in chroma_codes.into_iter().enumerate() {
                put::<TEN>(target, c_at + channel * sample_bytes, code);
            }
        }
    }
}
