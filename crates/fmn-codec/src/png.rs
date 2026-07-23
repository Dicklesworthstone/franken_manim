//! Owned PNG codec (§14.2): decode across the real ecosystem's full
//! matrix, encode for stills / sequences / the Studio stream.
//!
//! # Decode
//!
//! Grayscale ± alpha, truecolor ± alpha, indexed (+`tRNS`), bit depths
//! 1/2/4/8/16, Adam7 interlacing, all five filters — normalized to
//! **canonical RGBA8**. 16-bit samples quantize by rounding
//! (`(v·255 + 32767) / 65535`); sub-byte grayscale scales exactly
//! (×255, ×85, ×17).
//!
//! ## The gamma/sRGB chunk policy (defined precedence, documented)
//!
//! Pixel bytes are never resampled by the decoder. Color intent is
//! *reported* on the decoded image with this precedence: an `sRGB`
//! chunk wins over `gAMA`; absent both, samples are assumed
//! sRGB-encoded (the ecosystem's de-facto default). Consumers that
//! need linear light apply the transfer exactly once, in fmn-frame —
//! never here, never twice.
//!
//! ## The untrusted-input posture (§16.5, R14)
//!
//! Dimension and pixel-count limits are checked at `IHDR`; the exact
//! decompressed size is computed from the header geometry and declared
//! to the inflater **before** any decompression, so a bomb is refused
//! at the declared bound, not discovered at allocation. Chunk counts
//! are bounded, CRCs are verified, IDAT runs must be consecutive, and
//! unknown *critical* chunks are typed refusals.
//!
//! # Encode
//!
//! Canonical RGBA8 in, deterministic bytes out: per-row
//! minimum-sum-of-absolute-differences filter selection with fixed tie
//! order, the owned deterministic DEFLATE, and a fixed chunk sequence
//! (`IHDR`, `sRGB`, `gAMA`, `IDAT`, `IEND`) — the same image and level
//! produce the same file on every platform (self-goldens depend on it).

use crate::checksum::crc32;
use crate::deflate::{CompressionLevel, zlib_compress};
use crate::inflate::{InflateError, zlib_decompress};

/// Typed refusals of the PNG decoder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PngError {
    /// The signature bytes are not PNG's.
    NotPng,
    /// The stream ended mid-chunk.
    Truncated,
    /// A chunk CRC-32 check failed.
    ChunkCrc,
    /// More chunks than the declared budget.
    TooManyChunks {
        /// The configured chunk budget.
        limit: usize,
    },
    /// The image exceeds the declared pixel budget.
    TooLarge {
        /// The configured pixel budget.
        max_pixels: u64,
    },
    /// A malformed or contradictory IHDR field.
    BadIhdr(&'static str),
    /// A malformed PLTE chunk, or one missing where required.
    BadPalette(&'static str),
    /// A malformed tRNS chunk for the image's color type.
    BadTrns(&'static str),
    /// IDAT chunks must be consecutive; data was missing or scattered.
    BadIdat(&'static str),
    /// A chunk after IEND, or a malformed IEND.
    BadIend,
    /// An unknown chunk marked critical — skipping it would silently
    /// misrender, so it is refused by name.
    UnknownCritical([u8; 4]),
    /// A scanline filter byte outside 0..=4.
    BadFilter(u8),
    /// The pixel stream failed to decompress.
    Inflate(InflateError),
    /// The decompressed stream is not exactly the geometry's size.
    WrongDataSize {
        /// Bytes the header geometry requires.
        expected: usize,
        /// Bytes the stream actually inflated to.
        got: usize,
    },
}

impl std::fmt::Display for PngError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotPng => write!(f, "not a PNG (bad signature)"),
            Self::Truncated => write!(f, "png stream truncated mid-chunk"),
            Self::ChunkCrc => write!(f, "png chunk crc mismatch"),
            Self::TooManyChunks { limit } => {
                write!(f, "png exceeds the {limit}-chunk budget")
            }
            Self::TooLarge { max_pixels } => {
                write!(f, "png exceeds the {max_pixels}-pixel budget")
            }
            Self::BadIhdr(what) => write!(f, "malformed IHDR: {what}"),
            Self::BadPalette(what) => write!(f, "malformed palette: {what}"),
            Self::BadTrns(what) => write!(f, "malformed tRNS: {what}"),
            Self::BadIdat(what) => write!(f, "malformed IDAT run: {what}"),
            Self::BadIend => write!(f, "malformed or misplaced IEND"),
            Self::UnknownCritical(name) => {
                write!(f, "unknown critical chunk {:?}", name.map(|b| b as char))
            }
            Self::BadFilter(t) => write!(f, "scanline filter {t} outside 0..=4"),
            Self::Inflate(e) => write!(f, "pixel stream: {e}"),
            Self::WrongDataSize { expected, got } => write!(
                f,
                "decompressed pixel stream is {got} bytes, geometry requires {expected}"
            ),
        }
    }
}

impl std::error::Error for PngError {}

impl From<InflateError> for PngError {
    fn from(e: InflateError) -> Self {
        Self::Inflate(e)
    }
}

/// Decode resource budgets, declared before any work happens.
#[derive(Debug, Clone)]
pub struct PngLimits {
    /// Maximum `width × height` in pixels.
    pub max_pixels: u64,
    /// Maximum chunk count.
    pub max_chunks: usize,
}

impl Default for PngLimits {
    /// 268 megapixels (a 16384² frame) and 4096 chunks — far above any
    /// real asset, far below a bomb.
    fn default() -> Self {
        Self {
            max_pixels: 1 << 28,
            max_chunks: 4096,
        }
    }
}

/// The reported color intent (see the module docs for the precedence).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColorIntent {
    /// An `sRGB` chunk was present (rendering intent kept verbatim).
    Srgb {
        /// The declared rendering intent byte.
        intent: u8,
    },
    /// Only a `gAMA` chunk was present; value is gamma × 100000.
    Gamma {
        /// Encoded gamma × 100000, as stored.
        gamma_100000: u32,
    },
    /// Neither chunk: samples are assumed sRGB-encoded.
    AssumedSrgb,
}

/// A decoded PNG, normalized to canonical RGBA8 (tight rows, output
/// orientation — row 0 is the top row, D-23).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecodedPng {
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
    /// `width × height × 4` bytes, RGBA, row-major.
    pub rgba: Vec<u8>,
    /// The source's color type byte (0/2/3/4/6), for provenance.
    pub source_color_type: u8,
    /// The source's bit depth (1/2/4/8/16), for provenance.
    pub source_bit_depth: u8,
    /// The reported color intent.
    pub intent: ColorIntent,
}

const SIGNATURE: [u8; 8] = [137, 80, 78, 71, 13, 10, 26, 10];

/// Adam7 pass geometry: x origin, y origin, x step, y step.
const ADAM7: [(u32, u32, u32, u32); 7] = [
    (0, 0, 8, 8),
    (4, 0, 8, 8),
    (0, 4, 4, 8),
    (2, 0, 4, 4),
    (0, 2, 2, 4),
    (1, 0, 2, 2),
    (0, 1, 1, 2),
];

const fn channels(color_type: u8) -> u32 {
    match color_type {
        0 | 3 => 1,
        4 => 2,
        2 => 3,
        _ => 4,
    }
}

/// Bytes per scanline for `width` pixels (excluding the filter byte).
const fn row_bytes(width: u32, bits_per_pixel: u32) -> usize {
    ((width as u64 * bits_per_pixel as u64).div_ceil(8)) as usize
}

/// Filter-reconstruction step distance in whole bytes.
const fn filter_bpp(bits_per_pixel: u32) -> usize {
    bits_per_pixel.div_ceil(8) as usize
}

struct Ihdr {
    width: u32,
    height: u32,
    bit_depth: u8,
    color_type: u8,
    interlaced: bool,
}

impl Ihdr {
    fn parse(data: &[u8]) -> Result<Self, PngError> {
        if data.len() != 13 {
            return Err(PngError::BadIhdr("length"));
        }
        let width = u32::from_be_bytes([data[0], data[1], data[2], data[3]]);
        let height = u32::from_be_bytes([data[4], data[5], data[6], data[7]]);
        let (bit_depth, color_type) = (data[8], data[9]);
        if width == 0 || height == 0 || width > 0x7fff_ffff || height > 0x7fff_ffff {
            return Err(PngError::BadIhdr("dimensions"));
        }
        let depth_ok = match color_type {
            0 => matches!(bit_depth, 1 | 2 | 4 | 8 | 16),
            3 => matches!(bit_depth, 1 | 2 | 4 | 8),
            2 | 4 | 6 => matches!(bit_depth, 8 | 16),
            _ => return Err(PngError::BadIhdr("color type")),
        };
        if !depth_ok {
            return Err(PngError::BadIhdr("bit depth for color type"));
        }
        if data[10] != 0 {
            return Err(PngError::BadIhdr("compression method"));
        }
        if data[11] != 0 {
            return Err(PngError::BadIhdr("filter method"));
        }
        let interlaced = match data[12] {
            0 => false,
            1 => true,
            _ => return Err(PngError::BadIhdr("interlace method")),
        };
        Ok(Self {
            width,
            height,
            bit_depth,
            color_type,
            interlaced,
        })
    }

    const fn bits_per_pixel(&self) -> u32 {
        channels(self.color_type) * self.bit_depth as u32
    }

    /// The pass list actually present in the stream: `(x0, y0, xstep,
    /// ystep, pass_width, pass_height)`, empty passes excluded.
    fn passes(&self) -> Vec<(u32, u32, u32, u32, u32, u32)> {
        if self.interlaced {
            ADAM7
                .iter()
                .map(|&(x0, y0, xs, ys)| {
                    let w = self.width.saturating_sub(x0).div_ceil(xs);
                    let h = self.height.saturating_sub(y0).div_ceil(ys);
                    (x0, y0, xs, ys, w, h)
                })
                .filter(|&(.., w, h)| w > 0 && h > 0)
                .collect()
        } else {
            vec![(0, 0, 1, 1, self.width, self.height)]
        }
    }

    /// Exact byte size of the filtered pixel stream — the inflate
    /// budget, computed from geometry alone.
    fn raw_stream_size(&self) -> usize {
        let bpp = self.bits_per_pixel();
        self.passes()
            .iter()
            .map(|&(.., w, h)| h as usize * (1 + row_bytes(w, bpp)))
            .sum()
    }
}

/// Undo one scanline's filter in place. `prev` is the reconstructed
/// previous scanline of the same pass (empty for the first row).
fn unfilter_row(filter: u8, row: &mut [u8], prev: &[u8], bpp: usize) -> Result<(), PngError> {
    match filter {
        0 => {}
        1 => {
            for i in bpp..row.len() {
                row[i] = row[i].wrapping_add(row[i - bpp]);
            }
        }
        2 => {
            if !prev.is_empty() {
                for (byte, &up) in row.iter_mut().zip(prev) {
                    *byte = byte.wrapping_add(up);
                }
            }
        }
        3 => {
            for i in 0..row.len() {
                let left = if i >= bpp { u16::from(row[i - bpp]) } else { 0 };
                let up = if prev.is_empty() {
                    0
                } else {
                    u16::from(prev[i])
                };
                row[i] = row[i].wrapping_add(((left + up) / 2) as u8);
            }
        }
        4 => {
            for i in 0..row.len() {
                let a = if i >= bpp { i32::from(row[i - bpp]) } else { 0 };
                let b = if prev.is_empty() {
                    0
                } else {
                    i32::from(prev[i])
                };
                let c = if i >= bpp && !prev.is_empty() {
                    i32::from(prev[i - bpp])
                } else {
                    0
                };
                let p = a + b - c;
                let (pa, pb, pc) = ((p - a).abs(), (p - b).abs(), (p - c).abs());
                let predictor = if pa <= pb && pa <= pc {
                    a
                } else if pb <= pc {
                    b
                } else {
                    c
                };
                row[i] = row[i].wrapping_add(predictor as u8);
            }
        }
        t => return Err(PngError::BadFilter(t)),
    }
    Ok(())
}

/// Read sample `index` from a reconstructed scanline. Samples are
/// channel-interleaved; `index` counts samples, not pixels. Sub-byte
/// samples pack big-endian within a byte (leftmost pixel high).
fn read_sample(row: &[u8], index: usize, bit_depth: u8) -> u16 {
    match bit_depth {
        16 => u16::from_be_bytes([row[index * 2], row[index * 2 + 1]]),
        8 => u16::from(row[index]),
        d => {
            let per_byte = usize::from(8 / d);
            let byte = row[index / per_byte];
            let shift = 8 - ((index % per_byte) + 1) * usize::from(d);
            u16::from(byte >> shift) & ((1u16 << d) - 1)
        }
    }
}

/// Scale a source sample to 8 bits: exact expansion for sub-byte
/// depths, identity for 8, rounded quantization for 16.
fn scale_sample(v: u16, bit_depth: u8) -> u8 {
    match bit_depth {
        1 => (v * 255) as u8,
        2 => (v * 85) as u8,
        4 => (v * 17) as u8,
        8 => v as u8,
        _ => ((u32::from(v) * 255 + 32767) / 65535) as u8,
    }
}

/// The tRNS payload, interpreted per color type.
enum Transparency {
    None,
    /// Color type 3: per-palette-index alpha.
    Palette(Vec<u8>),
    /// Color type 0: the fully transparent gray sample (source depth).
    Gray(u16),
    /// Color type 2: the fully transparent RGB sample (source depth).
    Rgb([u16; 3]),
}

/// Decode a PNG to canonical RGBA8 under the given budgets.
pub fn decode(data: &[u8], limits: &PngLimits) -> Result<DecodedPng, PngError> {
    if data.len() < 8 || data[..8] != SIGNATURE {
        return Err(PngError::NotPng);
    }

    let mut cursor = 8usize;
    let mut chunk_count = 0usize;
    let mut ihdr: Option<Ihdr> = None;
    let mut palette: Option<Vec<[u8; 3]>> = None;
    let mut transparency = Transparency::None;
    let mut srgb: Option<u8> = None;
    let mut gama: Option<u32> = None;
    let mut idat: Vec<u8> = Vec::new();
    let mut idat_started = false;
    let mut idat_done = false;
    let mut iend = false;

    while cursor < data.len() {
        if iend {
            return Err(PngError::BadIend);
        }
        chunk_count += 1;
        if chunk_count > limits.max_chunks {
            return Err(PngError::TooManyChunks {
                limit: limits.max_chunks,
            });
        }
        if data.len() - cursor < 12 {
            return Err(PngError::Truncated);
        }
        let len = u32::from_be_bytes([
            data[cursor],
            data[cursor + 1],
            data[cursor + 2],
            data[cursor + 3],
        ]) as usize;
        if len > 0x7fff_ffff || data.len() - cursor - 12 < len {
            return Err(PngError::Truncated);
        }
        let name: [u8; 4] = [
            data[cursor + 4],
            data[cursor + 5],
            data[cursor + 6],
            data[cursor + 7],
        ];
        let body = &data[cursor + 8..cursor + 8 + len];
        let stored_crc = u32::from_be_bytes([
            data[cursor + 8 + len],
            data[cursor + 9 + len],
            data[cursor + 10 + len],
            data[cursor + 11 + len],
        ]);
        if crc32(&data[cursor + 4..cursor + 8 + len]) != stored_crc {
            return Err(PngError::ChunkCrc);
        }
        cursor += 12 + len;

        // IDAT chunks must form one consecutive run.
        if idat_started && !idat_done && &name != b"IDAT" {
            idat_done = true;
        }

        match &name {
            b"IHDR" => {
                if ihdr.is_some() || chunk_count != 1 {
                    return Err(PngError::BadIhdr("IHDR must be the first, only header"));
                }
                let parsed = Ihdr::parse(body)?;
                let pixels = u64::from(parsed.width) * u64::from(parsed.height);
                if pixels > limits.max_pixels {
                    return Err(PngError::TooLarge {
                        max_pixels: limits.max_pixels,
                    });
                }
                ihdr = Some(parsed);
            }
            b"PLTE" => {
                let header = ihdr.as_ref().ok_or(PngError::BadIhdr("missing"))?;
                if len == 0 || !len.is_multiple_of(3) || len / 3 > 256 {
                    return Err(PngError::BadPalette("entry count"));
                }
                if palette.is_some() || idat_started {
                    return Err(PngError::BadPalette("duplicate or late PLTE"));
                }
                if matches!(header.color_type, 0 | 4) {
                    return Err(PngError::BadPalette("PLTE forbidden for grayscale"));
                }
                palette = Some(body.as_chunks::<3>().0.to_vec());
            }
            b"tRNS" => {
                let header = ihdr.as_ref().ok_or(PngError::BadIhdr("missing"))?;
                if idat_started {
                    return Err(PngError::BadTrns("tRNS after IDAT"));
                }
                transparency = match header.color_type {
                    3 => {
                        let entries = palette
                            .as_ref()
                            .ok_or(PngError::BadTrns("tRNS before PLTE"))?
                            .len();
                        if len > entries {
                            return Err(PngError::BadTrns("more entries than palette"));
                        }
                        Transparency::Palette(body.to_vec())
                    }
                    0 => {
                        if len != 2 {
                            return Err(PngError::BadTrns("grayscale tRNS length"));
                        }
                        Transparency::Gray(u16::from_be_bytes([body[0], body[1]]))
                    }
                    2 => {
                        if len != 6 {
                            return Err(PngError::BadTrns("rgb tRNS length"));
                        }
                        Transparency::Rgb([
                            u16::from_be_bytes([body[0], body[1]]),
                            u16::from_be_bytes([body[2], body[3]]),
                            u16::from_be_bytes([body[4], body[5]]),
                        ])
                    }
                    _ => return Err(PngError::BadTrns("tRNS with an alpha color type")),
                };
            }
            b"sRGB" => {
                if len != 1 {
                    return Err(PngError::BadIhdr("sRGB length"));
                }
                srgb = Some(body[0]);
            }
            b"gAMA" => {
                if len != 4 {
                    return Err(PngError::BadIhdr("gAMA length"));
                }
                gama = Some(u32::from_be_bytes([body[0], body[1], body[2], body[3]]));
            }
            b"IDAT" => {
                if ihdr.is_none() {
                    return Err(PngError::BadIdat("IDAT before IHDR"));
                }
                if idat_done {
                    return Err(PngError::BadIdat("IDAT run is not consecutive"));
                }
                idat_started = true;
                idat.extend_from_slice(body);
            }
            b"IEND" => {
                if len != 0 {
                    return Err(PngError::BadIend);
                }
                iend = true;
            }
            _ => {
                // Ancillary (lowercase first letter) chunks are skipped;
                // unknown critical chunks are refused.
                if name[0] & 0x20 == 0 {
                    return Err(PngError::UnknownCritical(name));
                }
            }
        }
    }
    if !iend {
        return Err(PngError::Truncated);
    }
    let header = ihdr.ok_or(PngError::BadIhdr("missing"))?;
    if !idat_started {
        return Err(PngError::BadIdat("no IDAT"));
    }
    if header.color_type == 3 && palette.is_none() {
        return Err(PngError::BadPalette("indexed image without PLTE"));
    }

    // The budget is exact and declared before inflation begins.
    let expected = header.raw_stream_size();
    let raw = zlib_decompress(&idat, expected)?;
    if raw.len() != expected {
        return Err(PngError::WrongDataSize {
            expected,
            got: raw.len(),
        });
    }

    let intent = match (srgb, gama) {
        (Some(i), _) => ColorIntent::Srgb { intent: i },
        (None, Some(g)) => ColorIntent::Gamma { gamma_100000: g },
        (None, None) => ColorIntent::AssumedSrgb,
    };

    let mut rgba = vec![0u8; header.width as usize * header.height as usize * 4];
    let bpp_bits = header.bits_per_pixel();
    let bpp = filter_bpp(bpp_bits);
    let depth = header.bit_depth;
    let ch = channels(header.color_type) as usize;
    let palette = palette.unwrap_or_default();

    let mut offset = 0usize;
    for (x0, y0, xs, ys, pass_w, pass_h) in header.passes() {
        let line = row_bytes(pass_w, bpp_bits);
        let mut prev: Vec<u8> = Vec::new();
        for py in 0..pass_h {
            let filter = raw[offset];
            let mut row = raw[offset + 1..offset + 1 + line].to_vec();
            offset += 1 + line;
            unfilter_row(filter, &mut row, &prev, bpp)?;

            let out_y = y0 + py * ys;
            for px in 0..pass_w {
                let out_x = x0 + px * xs;
                let at = (out_y as usize * header.width as usize + out_x as usize) * 4;
                let base = px as usize * ch;
                let pixel: [u8; 4] = match header.color_type {
                    0 => {
                        let v = read_sample(&row, base, depth);
                        let g = scale_sample(v, depth);
                        let alpha = match transparency {
                            Transparency::Gray(t) if t == v => 0,
                            _ => 255,
                        };
                        [g, g, g, alpha]
                    }
                    2 => {
                        let r = read_sample(&row, base, depth);
                        let g = read_sample(&row, base + 1, depth);
                        let b = read_sample(&row, base + 2, depth);
                        let alpha = match transparency {
                            Transparency::Rgb(t) if t == [r, g, b] => 0,
                            _ => 255,
                        };
                        [
                            scale_sample(r, depth),
                            scale_sample(g, depth),
                            scale_sample(b, depth),
                            alpha,
                        ]
                    }
                    3 => {
                        let index = usize::from(read_sample(&row, base, depth));
                        let entry = palette
                            .get(index)
                            .ok_or(PngError::BadPalette("index out of range"))?;
                        let alpha = match &transparency {
                            Transparency::Palette(a) => a.get(index).copied().unwrap_or(255),
                            _ => 255,
                        };
                        [entry[0], entry[1], entry[2], alpha]
                    }
                    4 => {
                        let g = scale_sample(read_sample(&row, base, depth), depth);
                        let a = scale_sample(read_sample(&row, base + 1, depth), depth);
                        [g, g, g, a]
                    }
                    _ => [
                        scale_sample(read_sample(&row, base, depth), depth),
                        scale_sample(read_sample(&row, base + 1, depth), depth),
                        scale_sample(read_sample(&row, base + 2, depth), depth),
                        scale_sample(read_sample(&row, base + 3, depth), depth),
                    ],
                };
                rgba[at..at + 4].copy_from_slice(&pixel);
            }
            prev = row;
        }
    }

    Ok(DecodedPng {
        width: header.width,
        height: header.height,
        rgba,
        source_color_type: header.color_type,
        source_bit_depth: header.bit_depth,
        intent,
    })
}

/// Apply filter `filter` to `row` (with `prev` as the prior raw row)
/// into `out`.
fn apply_filter(filter: u8, row: &[u8], prev: &[u8], bpp: usize, out: &mut Vec<u8>) {
    out.clear();
    match filter {
        0 => out.extend_from_slice(row),
        1 => {
            for i in 0..row.len() {
                let left = if i >= bpp { row[i - bpp] } else { 0 };
                out.push(row[i].wrapping_sub(left));
            }
        }
        2 => {
            for i in 0..row.len() {
                let up = if prev.is_empty() { 0 } else { prev[i] };
                out.push(row[i].wrapping_sub(up));
            }
        }
        3 => {
            for i in 0..row.len() {
                let left = if i >= bpp { u16::from(row[i - bpp]) } else { 0 };
                let up = if prev.is_empty() {
                    0
                } else {
                    u16::from(prev[i])
                };
                out.push(row[i].wrapping_sub(((left + up) / 2) as u8));
            }
        }
        _ => {
            for i in 0..row.len() {
                let a = if i >= bpp { i32::from(row[i - bpp]) } else { 0 };
                let b = if prev.is_empty() {
                    0
                } else {
                    i32::from(prev[i])
                };
                let c = if i >= bpp && !prev.is_empty() {
                    i32::from(prev[i - bpp])
                } else {
                    0
                };
                let p = a + b - c;
                let (pa, pb, pc) = ((p - a).abs(), (p - b).abs(), (p - c).abs());
                let predictor = if pa <= pb && pa <= pc {
                    a
                } else if pb <= pc {
                    b
                } else {
                    c
                };
                out.push(row[i].wrapping_sub(predictor as u8));
            }
        }
    }
}

fn write_chunk(out: &mut Vec<u8>, name: &[u8; 4], body: &[u8]) {
    out.extend_from_slice(&(body.len() as u32).to_be_bytes());
    out.extend_from_slice(name);
    out.extend_from_slice(body);
    let mut tagged = Vec::with_capacity(4 + body.len());
    tagged.extend_from_slice(name);
    tagged.extend_from_slice(body);
    out.extend_from_slice(&crc32(&tagged).to_be_bytes());
}

/// Encode canonical RGBA8 (`width × height × 4` bytes, tight rows,
/// output orientation) as a deterministic PNG.
///
/// # Panics
///
/// Panics if `rgba.len() != width * height * 4` — that is a caller
/// bug, not an input condition.
#[must_use]
pub fn encode_rgba8(width: u32, height: u32, rgba: &[u8], level: CompressionLevel) -> Vec<u8> {
    assert_eq!(
        rgba.len(),
        width as usize * height as usize * 4,
        "rgba buffer does not match dimensions"
    );
    let line = width as usize * 4;
    let mut filtered = Vec::with_capacity((line + 1) * height as usize);
    let mut best: Vec<u8> = Vec::with_capacity(line);
    let mut candidate: Vec<u8> = Vec::with_capacity(line);
    for y in 0..height as usize {
        let row = &rgba[y * line..(y + 1) * line];
        let prev = if y == 0 {
            &[][..]
        } else {
            &rgba[(y - 1) * line..y * line]
        };
        // Minimum sum of absolute differences, fixed tie order 0..=4.
        let mut best_filter = 0u8;
        let mut best_score = u64::MAX;
        for filter in 0..=4u8 {
            apply_filter(filter, row, prev, 4, &mut candidate);
            let score: u64 = candidate
                .iter()
                .map(|&b| u64::from((b as i8).unsigned_abs()))
                .sum();
            if score < best_score {
                best_score = score;
                best_filter = filter;
                std::mem::swap(&mut best, &mut candidate);
            }
        }
        filtered.push(best_filter);
        filtered.extend_from_slice(&best);
    }

    let mut out = Vec::new();
    out.extend_from_slice(&SIGNATURE);
    let mut ihdr = Vec::with_capacity(13);
    ihdr.extend_from_slice(&width.to_be_bytes());
    ihdr.extend_from_slice(&height.to_be_bytes());
    ihdr.extend_from_slice(&[8, 6, 0, 0, 0]); // 8-bit RGBA, no interlace
    write_chunk(&mut out, b"IHDR", &ihdr);
    write_chunk(&mut out, b"sRGB", &[0]); // perceptual
    write_chunk(&mut out, b"gAMA", &45455u32.to_be_bytes());
    write_chunk(&mut out, b"IDAT", &zlib_compress(&filtered, level));
    write_chunk(&mut out, b"IEND", &[]);
    out
}
