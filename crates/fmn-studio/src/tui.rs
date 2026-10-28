//! Kitty-graphics and sixel terminal preview adapters.
//!
//! Both encoders write through an ordinary [`Write`] capability; they never
//! inspect ambient terminal state or spawn a helper. Kitty receives a
//! deterministic PNG in bounded base64 chunks. Sixel uses a fixed 6×6×6 RGB
//! cube and transparent pixels as unpainted background, so identical RGBA8
//! bytes produce identical terminal bytes.

use std::collections::BTreeMap;
use std::fmt;
use std::io::{Read, Write};
use std::sync::mpsc::SyncSender;

use fmn_codec::{CompressionLevel, PngError, PngLimits, decode_png, encode_rgba8};

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
        match self.protocol {
            TerminalProtocol::Kitty => {
                let png = encode_rgba8(width, height, rgba, CompressionLevel::Fast);
                write_kitty_png(writer, &png, self.limits)?;
            }
            TerminalProtocol::Sixel => {
                write_sixel(writer, width, height, rgba, self.limits)?;
            }
        }
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
        kitty_encoded_size(png.len(), self.limits)?;
        let max_pixels = u64::try_from(self.limits.max_pixels).unwrap_or(u64::MAX);
        decode_png(
            png,
            &PngLimits {
                max_pixels,
                ..PngLimits::default()
            },
        )?;
        write_kitty_png(writer, png, self.limits)?;
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

fn write_kitty_png(writer: &mut impl Write, png: &[u8], limits: TuiLimits) -> Result<(), TuiError> {
    const BASE64_WRITE_BUFFER_BYTES: usize = 4096;

    let (chunk_bytes, chunks, _) = kitty_encoded_size(png.len(), limits)?;
    let raw_chunk_bytes = (chunk_bytes / 4)
        .checked_mul(3)
        .ok_or(TuiError::EncodedSizeOverflow)?;
    let mut encoded = [0u8; BASE64_WRITE_BUFFER_BYTES];
    debug_assert_eq!(png.chunks(raw_chunk_bytes).len(), chunks);
    for (index, chunk) in png.chunks(raw_chunk_bytes).enumerate() {
        writer.write_all(b"\x1b_G")?;
        if index == 0 {
            writer.write_all(b"a=T,f=100,t=d,q=2,")?;
        }
        let more = index + 1 < chunks;
        writer.write_all(if more { b"m=1;" } else { b"m=0;" })?;
        let mut encoded_len = 0;
        for block in chunk.chunks(3) {
            if encoded_len == encoded.len() {
                writer.write_all(&encoded)?;
                encoded_len = 0;
            }
            encoded[encoded_len..encoded_len + 4].copy_from_slice(&base64_block(block));
            encoded_len += 4;
        }
        if encoded_len != 0 {
            writer.write_all(&encoded[..encoded_len])?;
        }
        writer.write_all(b"\x1b\\")?;
    }
    Ok(())
}

fn kitty_encoded_size(
    png_len: usize,
    limits: TuiLimits,
) -> Result<(usize, usize, usize), TuiError> {
    const FIRST_CHUNK_FRAMING_BYTES: usize =
        b"\x1b_G".len() + b"a=T,f=100,t=d,q=2,".len() + b"m=0;".len() + b"\x1b\\".len();
    const FOLLOWING_CHUNK_FRAMING_BYTES: usize = b"\x1b_G".len() + b"m=0;".len() + b"\x1b\\".len();

    let chunk_bytes = limits.kitty_chunk_bytes & !3;
    let base64_bytes = png_len
        .div_ceil(3)
        .checked_mul(4)
        .ok_or(TuiError::EncodedSizeOverflow)?;
    let chunks = base64_bytes.div_ceil(chunk_bytes);
    let framing_bytes = if chunks == 0 {
        0
    } else {
        (chunks - 1)
            .checked_mul(FOLLOWING_CHUNK_FRAMING_BYTES)
            .and_then(|following| following.checked_add(FIRST_CHUNK_FRAMING_BYTES))
            .ok_or(TuiError::EncodedSizeOverflow)?
    };
    let encoded_size = base64_bytes
        .checked_add(framing_bytes)
        .ok_or(TuiError::EncodedSizeOverflow)?;
    if encoded_size > limits.max_encoded_bytes {
        return Err(TuiError::EncodedLimit {
            limit: limits.max_encoded_bytes,
            needed: encoded_size,
        });
    }
    Ok((chunk_bytes, chunks, encoded_size))
}

fn write_sixel(
    writer: &mut impl Write,
    width: u32,
    height: u32,
    rgba: &[u8],
    limits: TuiLimits,
) -> Result<(), TuiError> {
    const PIXEL_WRITE_BUFFER_BYTES: usize = 4096;

    let (width_usize, height_usize, used) = sixel_preflight(width, height, rgba, limits)?;
    writer.write_all(b"\x1bP0;0;0q")?;
    write!(writer, "\"1;1;{width};{height}")?;
    for (index, present) in used.into_iter().enumerate() {
        if !present {
            continue;
        }
        let red = (index / 36) * 20;
        let green = ((index / 6) % 6) * 20;
        let blue = (index % 6) * 20;
        write!(writer, "#{index};2;{red};{green};{blue}")?;
    }

    let mut pixels = [0u8; PIXEL_WRITE_BUFFER_BYTES];
    for band_y in (0..height_usize).step_by(6) {
        let band_palette = band_palette(rgba, width_usize, height_usize, band_y);
        let mut any_color = false;
        for (color, present) in band_palette.into_iter().enumerate() {
            if !present {
                continue;
            }
            if any_color {
                writer.write_all(b"$")?;
            }
            any_color = true;
            write!(writer, "#{color}")?;
            let mut pixel_len = 0;
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
                pixels[pixel_len] = 63 + bits;
                pixel_len += 1;
                if pixel_len == pixels.len() {
                    writer.write_all(&pixels)?;
                    pixel_len = 0;
                }
            }
            if pixel_len != 0 {
                writer.write_all(&pixels[..pixel_len])?;
            }
        }
        writer.write_all(b"-")?;
    }
    writer.write_all(b"\x1b\\")?;
    Ok(())
}

fn sixel_preflight(
    width: u32,
    height: u32,
    rgba: &[u8],
    limits: TuiLimits,
) -> Result<(usize, usize, [bool; 216]), TuiError> {
    let width_usize = usize::try_from(width).map_err(|_| TuiError::DimensionOverflow)?;
    let height_usize = usize::try_from(height).map_err(|_| TuiError::DimensionOverflow)?;
    let mut used = [false; 216];
    for pixel in rgba.as_chunks::<4>().0 {
        if pixel[3] >= 128 {
            used[usize::from(cube_index(pixel[0], pixel[1], pixel[2]))] = true;
        }
    }

    let mut encoded_size = b"\x1bP0;0;0q".len();
    add_sixel_size(
        &mut encoded_size,
        b"\"1;1;".len() + decimal_digits(width_usize) + 1 + decimal_digits(height_usize),
    )?;
    for (index, present) in used.iter().copied().enumerate() {
        if !present {
            continue;
        }
        let red = (index / 36) * 20;
        let green = ((index / 6) % 6) * 20;
        let blue = (index % 6) * 20;
        add_sixel_size(
            &mut encoded_size,
            1 + decimal_digits(index)
                + b";2;".len()
                + decimal_digits(red)
                + 1
                + decimal_digits(green)
                + 1
                + decimal_digits(blue),
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
                add_sixel_size(&mut encoded_size, 1)?;
            }
            any_color = true;
            add_sixel_size(&mut encoded_size, 1 + decimal_digits(color) + width_usize)?;
        }
        add_sixel_size(&mut encoded_size, 1)?;
    }
    add_sixel_size(&mut encoded_size, b"\x1b\\".len())?;
    if encoded_size > limits.max_encoded_bytes {
        return Err(TuiError::EncodedLimit {
            limit: limits.max_encoded_bytes,
            needed: encoded_size,
        });
    }
    Ok((width_usize, height_usize, used))
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

fn add_sixel_size(total: &mut usize, additional: usize) -> Result<(), TuiError> {
    *total = total
        .checked_add(additional)
        .ok_or(TuiError::EncodedSizeOverflow)?;
    Ok(())
}

fn decimal_digits(mut value: usize) -> usize {
    let mut digits = 1;
    while value >= 10 {
        value /= 10;
        digits += 1;
    }
    digits
}

fn base64_block(bytes: &[u8]) -> [u8; 4] {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    debug_assert!((1..=3).contains(&bytes.len()));
    let first = bytes[0];
    let second = bytes.get(1).copied().unwrap_or(0);
    let third = bytes.get(2).copied().unwrap_or(0);
    [
        ALPHABET[usize::from(first >> 2)],
        ALPHABET[usize::from(((first & 0x03) << 4) | (second >> 4))],
        if bytes.len() > 1 {
            ALPHABET[usize::from(((second & 0x0f) << 2) | (third >> 6))]
        } else {
            b'='
        },
        if bytes.len() > 2 {
            ALPHABET[usize::from(third & 0x3f)]
        } else {
            b'='
        },
    ]
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
    /// The owned PNG decoder rejected the container or its pixel budget.
    InvalidPng(PngError),
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
            Self::InvalidPng(error) => write!(f, "terminal PNG is invalid: {error}"),
        }
    }
}

impl std::error::Error for TuiError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            Self::InvalidPng(error) => Some(error),
            _ => None,
        }
    }
}

impl From<std::io::Error> for TuiError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error)
    }
}

impl From<PngError> for TuiError {
    fn from(error: PngError) -> Self {
        Self::InvalidPng(error)
    }
}

/// The terminal is deliberately line-oriented. No raw-mode helper, terminal
/// mutation, or second external tool is needed; every command is followed by
/// Enter and the caller's terminal settings stay intact.
pub const TERMINAL_HELP: &str = "Studio terminal controls (type a command, then press Enter):\n\
  play / pause / toggle      play every nominal frame, or pause\n\
  next / prev                step one frame\n\
  step N / step Ns           step signed frames or seconds (e.g. step -1s)\n\
  seek N / seek Ns           seek a frame or time (e.g. seek 2.5s)\n\
  first / last               jump to a timeline endpoint\n\
  labels / label NAME        list or jump to a timeline label/bookmark\n\
  mark NAME                  bookmark this frame for this session\n\
  inspect [ID]               family tree, or one object's record fields\n\
  status / help / quit       show position/help, or stop (EOF also stops)\n\
Seeks and inspection are read-only previews; bookmarks are not saved to the scene.\n";

/// Maximum bytes retained from one line, including a trailing carriage return.
pub const MAX_TERMINAL_COMMAND_BYTES: usize = 1024;
/// Bound the queue between stdin and the worker-owning terminal loop.
pub const TERMINAL_COMMAND_QUEUE: usize = 32;
const MAX_TERMINAL_LABELS: usize = 512;
const MAX_TERMINAL_LABEL_BYTES: usize = 128;
const MAX_TERMINAL_TEXT_BYTES: usize = 16 * 1024;

/// A parsed, bounded terminal action. This is an Enter-based interface, not a
/// raw-key event stream or a route for injecting scene input events.
#[derive(Clone, Debug, PartialEq)]
pub enum TerminalCommand {
    Play,
    Pause,
    Toggle,
    StepFrames(i64),
    StepSeconds(f64),
    SeekFrame(u64),
    SeekSeconds(f64),
    First,
    Last,
    Label(String),
    Mark(String),
    Labels,
    Inspect(Option<usize>),
    Status,
    Help,
    Quit,
}

/// Parser diagnostics are static, so even malformed untrusted input cannot
/// turn a terminal diagnostic into an escape sequence or an unbounded echo.
pub type TerminalInput = Result<TerminalCommand, &'static str>;

impl TerminalCommand {
    /// Parse one line without accepting extra arguments or nonfinite times.
    pub fn parse(line: &str) -> TerminalInput {
        if line.len() > MAX_TERMINAL_COMMAND_BYTES {
            return Err("command exceeds the 1024-byte terminal limit");
        }
        if line
            .chars()
            .any(|ch| ch.is_control() && ch != '\t' && ch != '\r')
        {
            return Err("terminal commands cannot contain control characters");
        }
        let line = line.trim();
        let (verb, argument) = line.split_once(char::is_whitespace).unwrap_or((line, ""));
        let argument = argument.trim();
        let no_argument = || {
            if argument.is_empty() {
                Ok(())
            } else {
                Err("this command takes no arguments; type help")
            }
        };
        let simple = match verb {
            "play" | "p" => Some(Self::Play),
            "pause" => Some(Self::Pause),
            "toggle" => Some(Self::Toggle),
            "next" | "n" => Some(Self::StepFrames(1)),
            "prev" | "b" => Some(Self::StepFrames(-1)),
            "first" | "home" => Some(Self::First),
            "last" | "end" => Some(Self::Last),
            "labels" => Some(Self::Labels),
            "status" | "" => Some(Self::Status),
            "help" | "?" => Some(Self::Help),
            "quit" | "q" => Some(Self::Quit),
            _ => None,
        };
        if let Some(command) = simple {
            no_argument()?;
            return Ok(command);
        }
        match verb {
            "step" | "seek" => {
                if let Some(seconds) = argument.strip_suffix('s') {
                    let seconds = seconds
                        .parse::<f64>()
                        .map_err(|_| "time must be a finite number followed by s")?;
                    if !seconds.is_finite() || (verb == "seek" && seconds < 0.0) {
                        return Err("seek times must be nonnegative and all times must be finite");
                    }
                    Ok(if verb == "step" {
                        Self::StepSeconds(seconds)
                    } else {
                        Self::SeekSeconds(seconds)
                    })
                } else if verb == "step" {
                    argument
                        .parse()
                        .map(Self::StepFrames)
                        .map_err(|_| "step needs a signed frame count or seconds ending in s")
                } else {
                    argument
                        .parse()
                        .map(Self::SeekFrame)
                        .map_err(|_| "seek needs a nonnegative frame or seconds ending in s")
                }
            }
            "label" | "mark" => {
                if argument.is_empty()
                    || argument.len() > MAX_TERMINAL_LABEL_BYTES
                    || argument.chars().any(char::is_control)
                {
                    return Err("a label needs 1..128 bytes without control characters");
                }
                Ok(if verb == "label" {
                    Self::Label(argument.to_owned())
                } else {
                    Self::Mark(argument.to_owned())
                })
            }
            "inspect" | "i" => {
                if argument.is_empty() {
                    Ok(Self::Inspect(None))
                } else {
                    argument
                        .parse()
                        .map(|id| Self::Inspect(Some(id)))
                        .map_err(|_| "inspect takes one nonnegative object ID")
                }
            }
            _ => Err("unknown terminal command; type help"),
        }
    }
}

/// Feed a bounded queue from stdin or a scripted reader. Oversized lines are
/// discarded through their newline, never split into executable commands.
/// EOF processes an unterminated final line, then drops the producer normally.
/// A disconnected consumer or `quit` stops reading immediately.
pub fn read_terminal_commands(
    input: &mut impl Read,
    sender: &SyncSender<TerminalInput>,
) -> std::io::Result<()> {
    let mut chunk = [0_u8; 256];
    let mut line = Vec::with_capacity(MAX_TERMINAL_COMMAND_BYTES);
    let mut overlong = false;
    let send = |line: &[u8], overlong: bool| {
        let parsed = if overlong {
            Err("command exceeds the 1024-byte terminal limit; the whole line was discarded")
        } else {
            std::str::from_utf8(line)
                .map_err(|_| "terminal commands must be UTF-8")
                .and_then(TerminalCommand::parse)
        };
        let quit = matches!(parsed, Ok(TerminalCommand::Quit));
        sender.send(parsed).is_ok() && !quit
    };
    loop {
        let count = match input.read(&mut chunk) {
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            result => result?,
        };
        if count == 0 {
            if !line.is_empty() || overlong {
                send(&line, overlong);
            }
            return Ok(());
        }
        for byte in &chunk[..count] {
            if *byte == b'\n' {
                if !send(&line, overlong) {
                    return Ok(());
                }
                line.clear();
                overlong = false;
            } else if line.len() < MAX_TERMINAL_COMMAND_BYTES {
                line.push(*byte);
            } else {
                overlong = true;
            }
        }
    }
}

/// Read-only playback metadata projected from the current worker inspection.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TerminalView {
    pub frame: u64,
    pub frame_count: u64,
    pub fps: u32,
    pub presenter: bool,
    pub at_boundary: bool,
}

fn member<'a>(bytes: &'a [u8], key: &str) -> Result<&'a [u8], &'static str> {
    crate::protocol::json_object_member(bytes, key)
        .map_err(|_| "invalid terminal inspector JSON")?
        .ok_or("worker inspection omitted a required field")
}

fn number(bytes: &[u8]) -> Result<u64, &'static str> {
    std::str::from_utf8(bytes)
        .ok()
        .and_then(|text| text.parse().ok())
        .ok_or("worker inspection has an invalid integer")
}

impl TerminalView {
    fn validate(self) -> Result<Self, &'static str> {
        if self.fps == 0
            || self.frame_count == 0
            || self.frame >= self.frame_count
            || self.frame_count > (i64::MAX as u64) + 1
        {
            return Err("worker inspection has an invalid frame range or FPS");
        }
        Ok(self)
    }

    /// Read only structural root/view members, never lookalike text in an
    /// object's source excerpt. Unknown additive metadata is harmless.
    pub fn from_json(bytes: &[u8]) -> Result<Self, &'static str> {
        if bytes.len() > crate::ProtocolLimits::default().max_studio_data_bytes
            || std::str::from_utf8(bytes).is_err()
        {
            return Err("worker inspection exceeds the terminal protocol budget or is not UTF-8");
        }
        if number(member(bytes, "version")?)? != 1 {
            return Err("unsupported worker inspection version");
        }
        let view = member(bytes, "view")?;
        let frame = number(member(view, "frame_index")?)?;
        let frame_count = number(member(view, "frame_count")?)?;
        let fps =
            u32::try_from(number(member(view, "fps")?)?).map_err(|_| "worker FPS exceeds u32")?;
        let presenter = crate::protocol::json_object_member(bytes, "presenter")
            .map_err(|_| "invalid presenter metadata")?;
        let boolean = |value: &[u8]| match value {
            b"true" => Ok(true),
            b"false" => Ok(false),
            _ => Err("invalid presenter flag"),
        };
        Self {
            frame,
            frame_count,
            fps,
            presenter: presenter
                .map(|value| member(value, "enabled").and_then(boolean))
                .transpose()?
                .unwrap_or(false),
            at_boundary: presenter
                .map(|value| member(value, "at_boundary").and_then(boolean))
                .transpose()?
                .unwrap_or(false),
        }
        .validate()
    }
}

/// An operation for the existing Studio session; seeking does not edit a scene
/// or execute arbitrary terminal text in its worker.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TerminalAction {
    Seek(u64),
    Inspect(Option<usize>),
    Print(String),
    Quit,
}

/// Deterministic playback controls over acknowledged worker positions.
pub struct TerminalController {
    view: TerminalView,
    playing: bool,
    labels: BTreeMap<String, u64>,
}

impl TerminalController {
    pub fn new(view: TerminalView, labels: Vec<(String, u64)>) -> Result<Self, &'static str> {
        let view = view.validate()?;
        if labels.len() > MAX_TERMINAL_LABELS {
            return Err("terminal label budget is invalid");
        }
        let mut indexed = BTreeMap::new();
        for (name, frame) in labels {
            if name.is_empty()
                || name.len() > MAX_TERMINAL_LABEL_BYTES
                || name.chars().any(char::is_control)
                || frame >= view.frame_count
            {
                return Err("timeline label is outside the terminal name or frame budget");
            }
            indexed.entry(name).or_insert(frame);
        }
        Ok(Self {
            view,
            playing: false,
            labels: indexed,
        })
    }

    #[must_use]
    pub const fn view(&self) -> TerminalView {
        self.view
    }

    #[must_use]
    pub const fn playing(&self) -> bool {
        self.playing
    }

    pub fn pause(&mut self) {
        self.playing = false;
    }

    /// A seek changes the local position only after the worker acknowledges it.
    /// Presenter playback stops at each newly reached segment boundary.
    pub fn observe(&mut self, view: TerminalView) -> Result<(), &'static str> {
        let view = view.validate()?;
        if view.frame + 1 >= view.frame_count
            || (view.presenter && view.at_boundary && view.frame != self.view.frame)
        {
            self.pause();
        }
        self.view = view;
        Ok(())
    }

    #[must_use]
    pub fn next_frame(&self) -> Option<u64> {
        (self.playing && self.view.frame + 1 < self.view.frame_count).then_some(self.view.frame + 1)
    }

    #[must_use]
    pub fn status(&self) -> String {
        format!(
            "Frame {}/{} | {:.3} s | {}{}\n",
            self.view.frame,
            self.view.frame_count - 1,
            self.view.frame as f64 / f64::from(self.view.fps),
            if self.playing { "playing" } else { "paused" },
            if self.view.presenter && self.view.at_boundary {
                " | presenter boundary"
            } else {
                ""
            }
        )
    }

    fn seek(&mut self, frame: u64) -> Result<TerminalAction, &'static str> {
        if frame >= self.view.frame_count {
            return Err("seek is outside the timeline; use status to see its frame range");
        }
        self.pause();
        Ok(TerminalAction::Seek(frame))
    }

    fn step(&mut self, frames: i64) -> Result<TerminalAction, &'static str> {
        let frame = self
            .view
            .frame
            .saturating_add_signed(frames)
            .min(self.view.frame_count - 1);
        self.seek(frame)
    }

    fn seconds(&self, seconds: f64) -> Result<i64, &'static str> {
        let frames = seconds * f64::from(self.view.fps);
        if !frames.is_finite() || frames.abs() >= i64::MAX as f64 {
            return Err("time exceeds the terminal frame range");
        }
        Ok(frames.round() as i64)
    }

    pub fn apply(&mut self, command: TerminalCommand) -> Result<TerminalAction, &'static str> {
        match command {
            TerminalCommand::Play => self.playing = self.view.frame + 1 < self.view.frame_count,
            TerminalCommand::Pause => self.pause(),
            TerminalCommand::Toggle => {
                self.playing = !self.playing && self.view.frame + 1 < self.view.frame_count
            }
            TerminalCommand::StepFrames(frames) => return self.step(frames),
            TerminalCommand::StepSeconds(seconds) => return self.step(self.seconds(seconds)?),
            TerminalCommand::SeekFrame(frame) => return self.seek(frame),
            TerminalCommand::SeekSeconds(seconds) => {
                if seconds < 0.0 {
                    return Err("seek time is negative");
                }
                let frame =
                    u64::try_from(self.seconds(seconds)?).map_err(|_| "seek time is negative")?;
                return self.seek(frame);
            }
            TerminalCommand::First => return self.seek(0),
            TerminalCommand::Last => return self.seek(self.view.frame_count - 1),
            TerminalCommand::Label(name) => {
                let frame = self
                    .labels
                    .get(&name)
                    .copied()
                    .ok_or("unknown label; use labels or mark NAME")?;
                return self.seek(frame);
            }
            TerminalCommand::Mark(name) => {
                if name.is_empty()
                    || name.len() > MAX_TERMINAL_LABEL_BYTES
                    || name.chars().any(char::is_control)
                {
                    return Err("a label needs 1..128 bytes without control characters");
                }
                if self.labels.len() >= MAX_TERMINAL_LABELS && !self.labels.contains_key(&name) {
                    return Err("terminal label budget exhausted");
                }
                self.labels.insert(name, self.view.frame);
                return Ok(TerminalAction::Print(
                    "Bookmark saved for this terminal session.\n".to_owned(),
                ));
            }
            TerminalCommand::Labels => {
                let mut text = String::from("Timeline labels and session bookmarks:\n");
                for (name, frame) in &self.labels {
                    let line = format!(
                        "  {}: frame {frame}\n",
                        terminal_safe_text(name, MAX_TERMINAL_LABEL_BYTES)
                    );
                    if text.len() + line.len() > MAX_TERMINAL_TEXT_BYTES - 64 {
                        text.push_str("  ... label listing truncated\n");
                        break;
                    }
                    text.push_str(&line);
                }
                if self.labels.is_empty() {
                    text.push_str("  none; use mark NAME to bookmark this frame\n");
                }
                return Ok(TerminalAction::Print(text));
            }
            TerminalCommand::Inspect(id) => {
                self.pause();
                return Ok(TerminalAction::Inspect(id));
            }
            TerminalCommand::Help => return Ok(TerminalAction::Print(TERMINAL_HELP.to_owned())),
            TerminalCommand::Quit => {
                self.pause();
                return Ok(TerminalAction::Quit);
            }
            TerminalCommand::Status => {}
        }
        Ok(TerminalAction::Print(self.status()))
    }
}

/// Escape untrusted terminal text, including CSI/OSC introducers, and cap its
/// encoded byte length without ever splitting a UTF-8 character.
#[must_use]
pub fn terminal_safe_text(text: &str, maximum: usize) -> String {
    let mut output = String::new();
    for ch in text.chars() {
        let escaped = if ch.is_control()
            || ('\u{202a}'..='\u{202e}').contains(&ch)
            || ('\u{2066}'..='\u{2069}').contains(&ch)
        {
            ch.escape_default().to_string()
        } else {
            ch.to_string()
        };
        if output.len() + escaped.len() > maximum {
            break;
        }
        output.push_str(&escaped);
    }
    output
}

/// Bounded, read-only family/record view. Object IDs belong to this capture;
/// no handle from a previous frame is accepted as durable identity.
pub fn terminal_inspection(bytes: &[u8], selected: Option<usize>) -> Result<String, &'static str> {
    let view = TerminalView::from_json(bytes)?;
    let nodes = crate::protocol::json_array_values(member(bytes, "nodes")?, 50_000)
        .map_err(|_| "invalid inspector node array")?;
    let mut text = format!("Inspector at frame {} (capture-local IDs)\n", view.frame);
    let display = |value: &[u8], limit| {
        terminal_safe_text(std::str::from_utf8(value).unwrap_or("invalid UTF-8"), limit)
    };
    let mut shown = 0;
    for node in &nodes {
        let id = number(member(node, "id")?)?;
        if selected.is_some_and(|selected| selected as u64 != id) {
            continue;
        }
        if selected.is_none() && shown >= 32 {
            break;
        }
        let records = member(node, "records")?;
        text.push_str(&format!(
            "  #{id} root={} records={} parents={} children={}\n",
            display(member(node, "root")?, 8),
            display(member(records, "count")?, 24),
            display(member(node, "parents")?, 128),
            display(member(node, "children")?, 128)
        ));
        shown += 1;
        if selected.is_some() {
            for key in ["bounds", "uniforms", "source_span"] {
                text.push_str(&format!(
                    "    {key}: {}\n",
                    display(member(node, key)?, 1024)
                ));
            }
            let fields = crate::protocol::json_array_values(member(records, "fields")?, 128)
                .map_err(|_| "invalid record field array")?;
            for field in fields.iter().take(8) {
                text.push_str(&format!(
                    "    field {} width={} total_values={} values={}\n",
                    display(member(field, "name")?, 128),
                    display(member(field, "width")?, 24),
                    display(member(field, "total_values")?, 24),
                    display(member(field, "values")?, 512)
                ));
            }
            if fields.len() > 8 {
                text.push_str("    ... record fields truncated\n");
            }
            break;
        }
    }
    if selected.is_some() && shown == 0 {
        return Err("object ID is not present in this frame");
    }
    if selected.is_none() {
        text.push_str(&format!(
            "Showing {shown}/{} objects; inspect ID shows bounds, uniforms and record fields.\n",
            nodes.len()
        ));
    }
    if text.len() > MAX_TERMINAL_TEXT_BYTES {
        return Err("terminal inspector text budget exceeded");
    }
    Ok(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn view(frame: u64) -> TerminalView {
        TerminalView {
            frame,
            frame_count: 80,
            fps: 8,
            presenter: false,
            at_boundary: false,
        }
    }

    #[test]
    fn terminal_commands_parse_frames_times_and_labels_and_refuse_ambiguous_input() {
        assert_eq!(
            TerminalCommand::parse("step -3"),
            Ok(TerminalCommand::StepFrames(-3))
        );
        assert_eq!(
            TerminalCommand::parse(" step -0.5s\r"),
            Ok(TerminalCommand::StepSeconds(-0.5))
        );
        assert_eq!(
            TerminalCommand::parse("seek 2.5s"),
            Ok(TerminalCommand::SeekSeconds(2.5))
        );
        assert_eq!(
            TerminalCommand::parse("mark chapter one"),
            Ok(TerminalCommand::Mark("chapter one".into()))
        );
        assert_eq!(
            TerminalCommand::parse("inspect 42"),
            Ok(TerminalCommand::Inspect(Some(42)))
        );
        for bad in [
            "play extra",
            "seek -1",
            "seek -1s",
            "step NaNs",
            "step infs",
            "inspect -1",
            "label",
            "mark \u{1b}]0;bad",
            "seek 1 2",
        ] {
            assert!(TerminalCommand::parse(bad).is_err(), "accepted {bad:?}");
        }
        assert!(TERMINAL_HELP.contains("press Enter"));
    }

    #[test]
    fn terminal_stdin_discards_whole_oversized_lines_and_preserves_final_eof_command() {
        let mut script = b"quit ".to_vec();
        script.extend(std::iter::repeat_n(b'x', MAX_TERMINAL_COMMAND_BYTES));
        script.extend_from_slice(b"\n\xff\nseek 3\r\nstep -1");
        let (sender, receiver) = std::sync::mpsc::sync_channel(TERMINAL_COMMAND_QUEUE);
        read_terminal_commands(&mut std::io::Cursor::new(script), &sender).unwrap();
        drop(sender);
        let parsed = receiver.into_iter().collect::<Vec<_>>();
        assert_eq!(parsed.len(), 4);
        assert!(parsed[0].is_err());
        assert!(parsed[1].is_err());
        assert_eq!(parsed[2], Ok(TerminalCommand::SeekFrame(3)));
        assert_eq!(parsed[3], Ok(TerminalCommand::StepFrames(-1)));

        let (sender, receiver) = std::sync::mpsc::sync_channel(TERMINAL_COMMAND_QUEUE);
        read_terminal_commands(&mut &b"quit\nplay\n"[..], &sender).unwrap();
        drop(sender);
        assert_eq!(
            receiver.into_iter().collect::<Vec<_>>(),
            [Ok(TerminalCommand::Quit)]
        );

        let (sender, receiver) = std::sync::mpsc::sync_channel(TERMINAL_COMMAND_QUEUE);
        drop(receiver);
        let mut cancelled = std::io::Cursor::new(b"status\n".repeat(1024));
        read_terminal_commands(&mut cancelled, &sender).unwrap();
        assert!(cancelled.position() < cancelled.get_ref().len() as u64);
    }

    #[test]
    fn terminal_controller_changes_position_only_after_acknowledgement_and_bounds_steps() {
        let mut controller =
            TerminalController::new(view(10), vec![("chapter".into(), 30)]).unwrap();
        controller.apply(TerminalCommand::Play).unwrap();
        assert_eq!(controller.next_frame(), Some(11));
        assert!(controller.apply(TerminalCommand::SeekFrame(80)).is_err());
        assert_eq!(controller.view().frame, 10);
        assert!(
            controller.playing(),
            "a refused seek must not change playback state"
        );
        assert_eq!(
            controller.apply(TerminalCommand::SeekSeconds(2.5)).unwrap(),
            TerminalAction::Seek(20)
        );
        assert_eq!(controller.view().frame, 10);
        assert!(!controller.playing());
        controller.observe(view(20)).unwrap();
        assert_eq!(
            controller
                .apply(TerminalCommand::StepSeconds(-0.5))
                .unwrap(),
            TerminalAction::Seek(16)
        );
        assert_eq!(
            controller
                .apply(TerminalCommand::StepFrames(i64::MIN))
                .unwrap(),
            TerminalAction::Seek(0)
        );
        assert_eq!(
            controller
                .apply(TerminalCommand::StepFrames(i64::MAX))
                .unwrap(),
            TerminalAction::Seek(79)
        );
        assert_eq!(
            controller
                .apply(TerminalCommand::Label("chapter".into()))
                .unwrap(),
            TerminalAction::Seek(30)
        );
        controller
            .apply(TerminalCommand::Mark("remember".into()))
            .unwrap();
        controller.observe(view(40)).unwrap();
        assert_eq!(
            controller
                .apply(TerminalCommand::Label("remember".into()))
                .unwrap(),
            TerminalAction::Seek(20)
        );
        assert!(
            controller
                .apply(TerminalCommand::Mark("\u{1b}[2J".into()))
                .is_err()
        );
        assert!(
            controller
                .apply(TerminalCommand::SeekSeconds(f64::INFINITY))
                .is_err()
        );
        assert!(
            controller
                .apply(TerminalCommand::SeekSeconds(-0.001))
                .is_err()
        );
        assert!(
            controller
                .observe(TerminalView {
                    frame_count: 0,
                    ..view(0)
                })
                .is_err()
        );
        assert_eq!(controller.view().frame, 40);
        assert!(controller.apply(TerminalCommand::Inspect(Some(2))).is_ok());
        assert!(!controller.playing());

        let mut largest = TerminalController::new(
            TerminalView {
                frame: i64::MAX as u64,
                frame_count: (i64::MAX as u64) + 1,
                ..view(0)
            },
            Vec::new(),
        )
        .unwrap();
        assert_eq!(
            largest.apply(TerminalCommand::Last).unwrap(),
            TerminalAction::Seek(i64::MAX as u64)
        );
    }

    #[test]
    fn terminal_playback_stops_at_end_and_new_presenter_boundaries() {
        let mut initial = view(2);
        initial.presenter = true;
        initial.at_boundary = true;
        let mut controller = TerminalController::new(initial, Vec::new()).unwrap();
        controller.apply(TerminalCommand::Play).unwrap();
        controller.observe(initial).unwrap();
        assert_eq!(controller.next_frame(), Some(3));
        controller
            .observe(TerminalView {
                frame: 3,
                at_boundary: false,
                ..initial
            })
            .unwrap();
        assert!(controller.playing());
        controller
            .observe(TerminalView {
                frame: 7,
                ..initial
            })
            .unwrap();
        assert!(!controller.playing());
        controller.apply(TerminalCommand::Play).unwrap();
        controller
            .observe(TerminalView {
                frame: 79,
                at_boundary: false,
                ..initial
            })
            .unwrap();
        assert!(!controller.playing());
        controller.apply(TerminalCommand::Play).unwrap();
        assert_eq!(controller.next_frame(), None);
    }

    fn inspection(nodes: &str) -> Vec<u8> {
        format!(r#"{{"version":1,"view":{{"frame_index":2,"frame_count":80,"fps":8}},"nodes":{nodes},"source":"lookalike: \"frame_index\":999","presenter":{{"enabled":true,"at_boundary":false}}}}"#).into_bytes()
    }

    #[test]
    fn terminal_inspector_uses_structural_view_and_bounds_untrusted_text() {
        let json = inspection(
            r#"[{"id":7,"root":true,"parents":[],"children":[],"records":{"count":1,"fields":[{"name":"position","width":3,"total_values":3,"values":[0,1,2]}]},"bounds":[0,1],"uniforms":[],"source_span":{"file":"\\u001b]0;untrusted"}}]"#,
        );
        assert_eq!(
            TerminalView::from_json(&json).unwrap(),
            TerminalView {
                presenter: true,
                ..view(2)
            }
        );
        let list = terminal_inspection(&json, None).unwrap();
        assert!(list.contains("#7 root=true records=1"));
        let detail = terminal_inspection(&json, Some(7)).unwrap();
        assert!(detail.contains("field \"position\" width=3 total_values=3 values=[0,1,2]"));
        assert!(!detail.contains('\u{1b}'));
        assert!(terminal_inspection(&json, Some(8)).is_err());
        let duplicate_view =
            br#"{"version":1,"view":{"frame_index":2,"frame_index":4,"frame_count":80,"fps":8}}"#;
        assert!(TerminalView::from_json(duplicate_view).is_err());
        let escaped = terminal_safe_text("abc\u{1b}]0;title\u{7}\u{202e}é", 64);
        assert!(!escaped.chars().any(char::is_control));
        assert!(!escaped.contains('\u{202e}'));
        assert_eq!(terminal_safe_text("éé", 3), "é");

        let node = r#"{"id":7,"root":true,"parents":[],"children":[],"records":{"count":1}}"#;
        let nodes = format!("[{}]", vec![node; 100].join(","));
        let list = terminal_inspection(&inspection(&nodes), None).unwrap();
        assert!(list.contains("Showing 32/100 objects"));
        assert!(list.len() < MAX_TERMINAL_TEXT_BYTES);
    }

    #[derive(Default)]
    struct BoundedWriteObserver {
        bytes: Vec<u8>,
        max_write: usize,
        largest_write: usize,
    }

    impl Write for BoundedWriteObserver {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.largest_write = self.largest_write.max(bytes.len());
            if bytes.len() > self.max_write {
                return Err(std::io::Error::other("aggregate terminal write"));
            }
            self.bytes.extend_from_slice(bytes);
            Ok(bytes.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn base64_vectors_are_canonical() {
        assert_eq!(base64_block(b"f"), *b"Zg==");
        assert_eq!(base64_block(b"fo"), *b"Zm8=");
        assert_eq!(base64_block(b"foo"), *b"Zm9v");

        let limits = TuiLimits {
            kitty_chunk_bytes: 4,
            ..TuiLimits::default()
        };
        let mut framed = Vec::new();
        write_kitty_png(&mut framed, b"foobar", limits).unwrap();
        assert_eq!(
            framed,
            b"\x1b_Ga=T,f=100,t=d,q=2,m=1;Zm9v\x1b\\\x1b_Gm=0;YmFy\x1b\\"
        );
    }

    #[test]
    fn kitty_streams_png_as_bounded_writes() {
        let png = encode_rgba8(
            2,
            1,
            &[255, 0, 0, 255, 0, 255, 0, 255],
            CompressionLevel::Fast,
        );
        let limits = TuiLimits {
            kitty_chunk_bytes: 8,
            ..TuiLimits::default()
        };
        let preview = TerminalPreview::new(TerminalProtocol::Kitty, limits).unwrap();
        let mut expected = Vec::new();
        preview.write_png(&mut expected, &png).unwrap();

        let mut observed = BoundedWriteObserver {
            max_write: 32,
            ..BoundedWriteObserver::default()
        };
        preview.write_png(&mut observed, &png).unwrap();

        assert_eq!(observed.bytes, expected);
        assert!(observed.largest_write <= observed.max_write);
        assert!(expected.len() > observed.max_write);
    }

    #[test]
    fn kitty_size_preflight_rejects_arithmetic_overflow() {
        let limits = TuiLimits {
            max_encoded_bytes: usize::MAX,
            kitty_chunk_bytes: 4,
            ..TuiLimits::default()
        };
        assert!(matches!(
            kitty_encoded_size(usize::MAX, limits),
            Err(TuiError::EncodedSizeOverflow)
        ));
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

    #[test]
    fn sixel_streams_as_bounded_writes_after_exact_preflight() {
        let rgba = [255, 0, 0, 255].repeat(64);
        let generous = TerminalPreview::new(TerminalProtocol::Sixel, TuiLimits::default()).unwrap();
        let mut expected = Vec::new();
        generous.write_rgba8(&mut expected, 64, 1, &rgba).unwrap();

        let exact = TerminalPreview::new(
            TerminalProtocol::Sixel,
            TuiLimits {
                max_pixels: 64,
                max_encoded_bytes: expected.len(),
                ..TuiLimits::default()
            },
        )
        .unwrap();
        let mut observed = BoundedWriteObserver {
            max_write: 64,
            ..BoundedWriteObserver::default()
        };
        exact.write_rgba8(&mut observed, 64, 1, &rgba).unwrap();
        assert_eq!(observed.bytes, expected);
        assert!(observed.largest_write <= observed.max_write);
        assert!(expected.len() > observed.max_write);

        let one_byte_short = TerminalPreview::new(
            TerminalProtocol::Sixel,
            TuiLimits {
                max_pixels: 64,
                max_encoded_bytes: expected.len() - 1,
                ..TuiLimits::default()
            },
        )
        .unwrap();
        let mut refused = BoundedWriteObserver {
            max_write: 64,
            ..BoundedWriteObserver::default()
        };
        assert!(matches!(
            one_byte_short.write_rgba8(&mut refused, 64, 1, &rgba),
            Err(TuiError::EncodedLimit { limit, needed })
                if limit == expected.len() - 1 && needed == expected.len()
        ));
        assert!(refused.bytes.is_empty());
    }
}
