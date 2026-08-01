//! Kitty-graphics and sixel terminal preview adapters.
//!
//! Both encoders write through an ordinary [`Write`] capability; they never
//! inspect ambient terminal state or spawn a helper. Kitty receives a
//! deterministic PNG in bounded base64 chunks. Sixel uses a fixed 6×6×6 RGB
//! cube and transparent pixels as unpainted background, so identical RGBA8
//! bytes produce identical terminal bytes.

use std::fmt;
use std::io::Write;

use fmn_codec::{CompressionLevel, encode_rgba8};

/// Terminal image protocol.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TerminalProtocol {
    /// Kitty graphics protocol, PNG payload.
    Kitty,
    /// DEC sixel with a fixed 216-color cube.
    Sixel,
}

/// Terminal encoding budgets.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TuiLimits {
    /// Maximum source pixels.
    pub max_pixels: usize,
    /// Maximum terminal escape-sequence bytes.
    pub max_encoded_bytes: usize,
    /// Maximum base64 characters in one Kitty application command.
    pub kitty_chunk_bytes: usize,
}

impl Default for TuiLimits {
    fn default() -> Self {
        Self {
            max_pixels: 3840 * 2160,
            max_encoded_bytes: 128 * 1024 * 1024,
            kitty_chunk_bytes: 4096,
        }
    }
}

impl TuiLimits {
    fn validate(self) -> Result<Self, TuiError> {
        if self.max_pixels == 0 || self.max_encoded_bytes == 0 || self.kitty_chunk_bytes < 4 {
            Err(TuiError::InvalidLimits)
        } else {
            Ok(self)
        }
    }
}

/// Bounded terminal preview encoder.
#[derive(Clone, Copy, Debug)]
pub struct TerminalPreview {
    protocol: TerminalProtocol,
    limits: TuiLimits,
}

impl TerminalPreview {
    /// Construct an encoder.
    pub fn new(protocol: TerminalProtocol, limits: TuiLimits) -> Result<Self, TuiError> {
        Ok(Self {
            protocol,
            limits: limits.validate()?,
        })
    }

    /// Selected terminal protocol.
    #[must_use]
    pub const fn protocol(&self) -> TerminalProtocol {
        self.protocol
    }

    /// Encode and write a tight top-row-first RGBA8 frame.
    pub fn write_rgba8(
        &self,
        writer: &mut impl Write,
        width: u32,
        height: u32,
        rgba: &[u8],
    ) -> Result<(), TuiError> {
        validate_rgba(width, height, rgba, self.limits)?;
        let encoded = match self.protocol {
            TerminalProtocol::Kitty => {
                let png = encode_rgba8(width, height, rgba, CompressionLevel::Fast);
                encode_kitty_png(&png, self.limits)?
            }
            TerminalProtocol::Sixel => encode_sixel(width, height, rgba, self.limits)?,
        };
        writer.write_all(&encoded)?;
        writer.flush()?;
        Ok(())
    }

    /// Write already-encoded PNG through Kitty.
    pub fn write_png(&self, writer: &mut impl Write, png: &[u8]) -> Result<(), TuiError> {
        if self.protocol != TerminalProtocol::Kitty {
            return Err(TuiError::PngRequiresKitty);
        }
        if !png.starts_with(b"\x89PNG\r\n\x1a\n") {
            return Err(TuiError::InvalidPngSignature);
        }
        let encoded = encode_kitty_png(png, self.limits)?;
        writer.write_all(&encoded)?;
        writer.flush()?;
        Ok(())
    }
}

fn validate_rgba(width: u32, height: u32, rgba: &[u8], limits: TuiLimits) -> Result<(), TuiError> {
    if width == 0 || height == 0 {
        return Err(TuiError::ZeroDimensions);
    }
    let pixels = usize::try_from(width)
        .ok()
        .and_then(|width| {
            usize::try_from(height)
                .ok()
                .and_then(|height| width.checked_mul(height))
        })
        .ok_or(TuiError::DimensionOverflow)?;
    if pixels > limits.max_pixels {
        return Err(TuiError::PixelLimit {
            limit: limits.max_pixels,
            needed: pixels,
        });
    }
    let expected = pixels.checked_mul(4).ok_or(TuiError::DimensionOverflow)?;
    if rgba.len() != expected {
        return Err(TuiError::RgbaLength {
            expected,
            found: rgba.len(),
        });
    }
    Ok(())
}

fn encode_kitty_png(png: &[u8], limits: TuiLimits) -> Result<Vec<u8>, TuiError> {
    let chunk_bytes = limits.kitty_chunk_bytes & !3;
    let base64_bytes = png
        .len()
        .div_ceil(3)
        .checked_mul(4)
        .ok_or(TuiError::EncodedSizeOverflow)?;
    let chunks = base64_bytes.div_ceil(chunk_bytes);
    let framing_bytes = chunks
        .checked_mul(32)
        .ok_or(TuiError::EncodedSizeOverflow)?;
    let estimated = base64_bytes
        .checked_add(framing_bytes)
        .ok_or(TuiError::EncodedSizeOverflow)?;
    if estimated > limits.max_encoded_bytes {
        return Err(TuiError::EncodedLimit {
            limit: limits.max_encoded_bytes,
            needed: estimated,
        });
    }
    let base64 = base64(png);
    let mut out = Vec::with_capacity(estimated);
    for (index, chunk) in base64.as_bytes().chunks(chunk_bytes).enumerate() {
        out.extend_from_slice(b"\x1b_G");
        if index == 0 {
            out.extend_from_slice(b"a=T,f=100,t=d,q=2,");
        }
        let more = index + 1 < chunks;
        out.extend_from_slice(if more { b"m=1;" } else { b"m=0;" });
        out.extend_from_slice(chunk);
        out.extend_from_slice(b"\x1b\\");
    }
    if out.len() > limits.max_encoded_bytes {
        return Err(TuiError::EncodedLimit {
            limit: limits.max_encoded_bytes,
            needed: out.len(),
        });
    }
    Ok(out)
}

fn encode_sixel(
    width: u32,
    height: u32,
    rgba: &[u8],
    limits: TuiLimits,
) -> Result<Vec<u8>, TuiError> {
    let width_usize = usize::try_from(width).map_err(|_| TuiError::DimensionOverflow)?;
    let height_usize = usize::try_from(height).map_err(|_| TuiError::DimensionOverflow)?;
    let mut used = [false; 216];
    for pixel in rgba.as_chunks::<4>().0 {
        if pixel[3] >= 128 {
            used[usize::from(cube_index(pixel[0], pixel[1], pixel[2]))] = true;
        }
    }

    let mut out = Vec::new();
    extend_bounded(&mut out, b"\x1bP0;0;0q", limits)?;
    extend_bounded(
        &mut out,
        format!("\"1;1;{width};{height}").as_bytes(),
        limits,
    )?;
    for (index, present) in used.iter().copied().enumerate() {
        if !present {
            continue;
        }
        let red = (index / 36) * 20;
        let green = ((index / 6) % 6) * 20;
        let blue = (index % 6) * 20;
        extend_bounded(
            &mut out,
            format!("#{index};2;{red};{green};{blue}").as_bytes(),
            limits,
        )?;
    }

    for band_y in (0..height_usize).step_by(6) {
        let band_palette = band_palette(rgba, width_usize, height_usize, band_y);
        let mut any_color = false;
        for (color, present) in band_palette.into_iter().enumerate() {
            if !present {
                continue;
            }
            if any_color {
                push_bounded(&mut out, b'$', limits)?;
            }
            any_color = true;
            extend_bounded(&mut out, format!("#{color}").as_bytes(), limits)?;
            for x in 0..width_usize {
                let mut bits = 0u8;
                for bit in 0..6 {
                    let y = band_y + bit;
                    if y >= height_usize {
                        break;
                    }
                    let at = (y * width_usize + x) * 4;
                    let pixel = &rgba[at..at + 4];
                    if pixel[3] >= 128
                        && usize::from(cube_index(pixel[0], pixel[1], pixel[2])) == color
                    {
                        bits |= 1 << bit;
                    }
                }
                push_bounded(&mut out, 63 + bits, limits)?;
            }
        }
        push_bounded(&mut out, b'-', limits)?;
    }
    extend_bounded(&mut out, b"\x1b\\", limits)?;
    Ok(out)
}

fn band_palette(rgba: &[u8], width: usize, height: usize, band_y: usize) -> [bool; 216] {
    let mut palette = [false; 216];
    for y in band_y..band_y.saturating_add(6).min(height) {
        for x in 0..width {
            let at = (y * width + x) * 4;
            let pixel = &rgba[at..at + 4];
            if pixel[3] >= 128 {
                palette[usize::from(cube_index(pixel[0], pixel[1], pixel[2]))] = true;
            }
        }
    }
    palette
}

const fn cube_index(red: u8, green: u8, blue: u8) -> u8 {
    let red = red / 51;
    let green = green / 51;
    let blue = blue / 51;
    red * 36 + green * 6 + blue
}

fn extend_bounded(out: &mut Vec<u8>, bytes: &[u8], limits: TuiLimits) -> Result<(), TuiError> {
    let needed = out
        .len()
        .checked_add(bytes.len())
        .ok_or(TuiError::EncodedSizeOverflow)?;
    if needed > limits.max_encoded_bytes {
        return Err(TuiError::EncodedLimit {
            limit: limits.max_encoded_bytes,
            needed,
        });
    }
    out.extend_from_slice(bytes);
    Ok(())
}

fn push_bounded(out: &mut Vec<u8>, byte: u8, limits: TuiLimits) -> Result<(), TuiError> {
    extend_bounded(out, &[byte], limits)
}

fn base64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let first = chunk[0];
        let second = chunk.get(1).copied().unwrap_or(0);
        let third = chunk.get(2).copied().unwrap_or(0);
        out.push(char::from(ALPHABET[usize::from(first >> 2)]));
        out.push(char::from(
            ALPHABET[usize::from(((first & 0x03) << 4) | (second >> 4))],
        ));
        if chunk.len() > 1 {
            out.push(char::from(
                ALPHABET[usize::from(((second & 0x0f) << 2) | (third >> 6))],
            ));
        } else {
            out.push('=');
        }
        if chunk.len() > 2 {
            out.push(char::from(ALPHABET[usize::from(third & 0x3f)]));
        } else {
            out.push('=');
        }
    }
    out
}

/// Terminal adapter failure.
#[derive(Debug)]
pub enum TuiError {
    /// I/O failure.
    Io(std::io::Error),
    /// At least one ceiling was zero or unusable.
    InvalidLimits,
    /// Frame width or height was zero.
    ZeroDimensions,
    /// Dimension arithmetic overflowed.
    DimensionOverflow,
    /// Source frame exceeded the pixel ceiling.
    PixelLimit {
        /// Ceiling.
        limit: usize,
        /// Requested pixels.
        needed: usize,
    },
    /// RGBA byte count did not match the dimensions.
    RgbaLength {
        /// Required bytes.
        expected: usize,
        /// Supplied bytes.
        found: usize,
    },
    /// Encoded-size arithmetic overflowed.
    EncodedSizeOverflow,
    /// Terminal output exceeded its ceiling.
    EncodedLimit {
        /// Ceiling.
        limit: usize,
        /// Bytes required so far.
        needed: usize,
    },
    /// A PNG was passed to a sixel encoder.
    PngRequiresKitty,
    /// Input did not begin with the PNG signature.
    InvalidPngSignature,
}

impl fmt::Display for TuiError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => write!(f, "terminal preview I/O: {error}"),
            Self::InvalidLimits => f.write_str("terminal preview limits must be nonzero"),
            Self::ZeroDimensions => f.write_str("terminal preview dimensions must be nonzero"),
            Self::DimensionOverflow => f.write_str("terminal preview dimensions overflow usize"),
            Self::PixelLimit { limit, needed } => {
                write!(
                    f,
                    "terminal frame has {needed} pixels, over the {limit}-pixel limit"
                )
            }
            Self::RgbaLength { expected, found } => {
                write!(
                    f,
                    "terminal RGBA frame has {found} bytes, expected {expected}"
                )
            }
            Self::EncodedSizeOverflow => f.write_str("terminal output size overflow"),
            Self::EncodedLimit { limit, needed } => {
                write!(
                    f,
                    "terminal output needs {needed} bytes, over the {limit}-byte limit"
                )
            }
            Self::PngRequiresKitty => f.write_str("pre-encoded PNG output requires Kitty graphics"),
            Self::InvalidPngSignature => f.write_str("terminal PNG has an invalid signature"),
        }
    }
}

impl std::error::Error for TuiError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            _ => None,
        }
    }
}

impl From<std::io::Error> for TuiError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_vectors_are_canonical() {
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
    }

    #[test]
    fn kitty_and_sixel_have_complete_escape_boundaries() {
        let rgba = [255, 0, 0, 255, 0, 255, 0, 255];
        let mut kitty = Vec::new();
        TerminalPreview::new(TerminalProtocol::Kitty, TuiLimits::default())
            .unwrap()
            .write_rgba8(&mut kitty, 2, 1, &rgba)
            .unwrap();
        assert!(kitty.starts_with(b"\x1b_Ga=T,f=100,t=d,q=2,"));
        assert!(kitty.ends_with(b"\x1b\\"));

        let mut sixel = Vec::new();
        TerminalPreview::new(TerminalProtocol::Sixel, TuiLimits::default())
            .unwrap()
            .write_rgba8(&mut sixel, 2, 1, &rgba)
            .unwrap();
        assert!(sixel.starts_with(b"\x1bP0;0;0q"));
        assert!(sixel.ends_with(b"\x1b\\"));
    }

    #[test]
    fn sixel_palette_discovery_is_band_local_and_byte_stable() {
        let mut rgba = Vec::new();
        for row in 0..12 {
            let pixel = if row < 6 {
                [255, 0, 0, 255]
            } else {
                [0, 255, 0, 255]
            };
            rgba.extend_from_slice(&pixel);
        }

        let first = band_palette(&rgba, 1, 12, 0);
        let second = band_palette(&rgba, 1, 12, 6);
        assert_eq!(first.into_iter().filter(|present| *present).count(), 1);
        assert!(first[180]);
        assert_eq!(second.into_iter().filter(|present| *present).count(), 1);
        assert!(second[30]);

        let mut sixel = Vec::new();
        TerminalPreview::new(TerminalProtocol::Sixel, TuiLimits::default())
            .unwrap()
            .write_rgba8(&mut sixel, 1, 12, &rgba)
            .unwrap();
        assert_eq!(
            sixel,
            b"\x1bP0;0;0q\"1;1;1;12#30;2;0;100;0#180;2;100;0;0#180~-#30~-\x1b\\"
        );
    }
}
