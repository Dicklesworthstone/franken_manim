//! Owned `.npy` v1.0 fixture interchange (§16.3 plane 1, fm-xb3).
//!
//! Structural fixtures against the Reference travel as NumPy `.npy` arrays:
//! the generation scripts (`scripts/gen_*.py`) emit them with `np.save`, and
//! the Gauntlet reads them here. This is a deliberately strict, deliberately
//! small reader/writer for the interchange subset the fixtures use:
//!
//! - format version 1.0 (2.0 headers are accepted on read),
//! - little-endian `<f8`, `<f4`, `<i8` dtypes only,
//! - C order only (`fortran_order: False`),
//! - 1 to 8 dimensions, element count checked exactly against the payload.
//!
//! Anything else is a precise, named error — fixtures are inputs, and a
//! malformed header must never become an allocation bomb (§16.5): the header
//! is capped, dimension counts are capped, and every size computation is
//! checked arithmetic.
//!
//! The governed closure (D1) is why this is owned: fnp-io is the designated
//! provider once the FrankenSuite is consumable from SUITE.lock, and this
//! module's surface is kept small so that migration (tracked as its own bead)
//! is a swap of internals, not a test rewrite.

use std::fmt;

/// Hard cap on the declared header length. numpy v1.0 headers are u16-sized
/// anyway; v2.0 declares u32 and this cap is what keeps that honest.
const MAX_HEADER_LEN: usize = 64 * 1024;
/// Hard cap on dimensionality; fixture arrays are 1-D or 2-D in practice.
const MAX_DIMS: usize = 8;
/// The six-byte magic every `.npy` file starts with.
const MAGIC: &[u8; 6] = b"\x93NUMPY";

/// The element type of an interchange array.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum DType {
    /// `<f8`: little-endian IEEE-754 double.
    F64,
    /// `<f4`: little-endian IEEE-754 single.
    F32,
    /// `<i8`: little-endian signed 64-bit integer.
    I64,
}

impl DType {
    /// The numpy descr string for this dtype.
    #[must_use]
    pub fn descr(self) -> &'static str {
        match self {
            Self::F64 => "<f8",
            Self::F32 => "<f4",
            Self::I64 => "<i8",
        }
    }

    /// Element size in bytes.
    #[must_use]
    pub fn size(self) -> usize {
        match self {
            Self::F64 | Self::I64 => 8,
            Self::F32 => 4,
        }
    }
}

/// The payload of an interchange array, in C (row-major) order.
#[derive(Clone, PartialEq, Debug)]
pub enum NpyData {
    /// `<f8` elements.
    F64(Vec<f64>),
    /// `<f4` elements.
    F32(Vec<f32>),
    /// `<i8` elements.
    I64(Vec<i64>),
}

impl NpyData {
    /// The dtype of this payload.
    #[must_use]
    pub fn dtype(&self) -> DType {
        match self {
            Self::F64(_) => DType::F64,
            Self::F32(_) => DType::F32,
            Self::I64(_) => DType::I64,
        }
    }

    /// Number of elements.
    #[must_use]
    pub fn len(&self) -> usize {
        match self {
            Self::F64(v) => v.len(),
            Self::F32(v) => v.len(),
            Self::I64(v) => v.len(),
        }
    }

    /// Whether the payload is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// A decoded (or to-be-encoded) `.npy` array: shape plus C-order payload.
#[derive(Clone, PartialEq, Debug)]
pub struct NpyArray {
    /// The array shape; product equals the payload length.
    pub shape: Vec<usize>,
    /// The payload.
    pub data: NpyData,
}

impl NpyArray {
    /// Construct an array, checking that the shape's element count matches
    /// the payload length and respects the dimensionality cap.
    ///
    /// # Errors
    /// [`NpyError::TooManyDims`], [`NpyError::Overflow`], or
    /// [`NpyError::DataLength`] when shape and payload disagree.
    pub fn new(shape: Vec<usize>, data: NpyData) -> Result<Self, NpyError> {
        let count = element_count(&shape)?;
        if count != data.len() {
            return Err(NpyError::DataLength {
                expected: count,
                actual: data.len(),
            });
        }
        Ok(Self { shape, data })
    }

    /// View the payload as `&[f64]`, if that is its dtype.
    #[must_use]
    pub fn as_f64(&self) -> Option<&[f64]> {
        match &self.data {
            NpyData::F64(v) => Some(v),
            _ => None,
        }
    }

    /// View the payload as `&[f32]`, if that is its dtype.
    #[must_use]
    pub fn as_f32(&self) -> Option<&[f32]> {
        match &self.data {
            NpyData::F32(v) => Some(v),
            _ => None,
        }
    }

    /// View the payload as `&[i64]`, if that is its dtype.
    #[must_use]
    pub fn as_i64(&self) -> Option<&[i64]> {
        match &self.data {
            NpyData::I64(v) => Some(v),
            _ => None,
        }
    }

    /// Interpret a `(n, 3)` f64 array as a point run — the shape constructor
    /// point-array fixtures use.
    ///
    /// # Errors
    /// [`NpyError::NotPoints`] unless the array is exactly 2-D with a
    /// trailing dimension of 3 and dtype `<f8`.
    pub fn to_points(&self) -> Result<Vec<[f64; 3]>, NpyError> {
        let Some(flat) = self.as_f64() else {
            return Err(NpyError::NotPoints {
                detail: format!("dtype is {:?}, expected <f8", self.data.dtype()),
            });
        };
        let &[_, 3] = self.shape.as_slice() else {
            return Err(NpyError::NotPoints {
                detail: format!("shape is {:?}, expected (n, 3)", self.shape),
            });
        };
        let (chunks, _rem) = flat.as_chunks::<3>();
        Ok(chunks.to_vec())
    }
}

/// A precise interchange failure.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum NpyError {
    /// The file is shorter than the structure it declares.
    Truncated {
        /// What was being read when the bytes ran out.
        reading: &'static str,
    },
    /// The six-byte magic is absent.
    BadMagic,
    /// A format version other than 1.0 / 2.0.
    UnsupportedVersion {
        /// Declared major.
        major: u8,
        /// Declared minor.
        minor: u8,
    },
    /// The declared header length exceeds the cap.
    HeaderTooLarge {
        /// Declared length.
        len: usize,
        /// The cap ([`MAX_HEADER_LEN`]).
        max: usize,
    },
    /// The header is not ASCII or its dict does not parse.
    Header {
        /// What was wrong.
        detail: String,
    },
    /// A descr outside the interchange subset.
    UnsupportedDescr {
        /// The descr string found.
        descr: String,
    },
    /// `fortran_order: True` — the interchange subset is C order only.
    FortranOrder,
    /// More dimensions than the cap.
    TooManyDims {
        /// Declared dimensionality.
        dims: usize,
        /// The cap ([`MAX_DIMS`]).
        max: usize,
    },
    /// Shape-product or size arithmetic overflowed.
    Overflow,
    /// Payload length disagrees with the declared shape.
    DataLength {
        /// Elements the shape declares.
        expected: usize,
        /// Elements the payload holds.
        actual: usize,
    },
    /// [`NpyArray::to_points`] on an array that is not `(n, 3)` `<f8`.
    NotPoints {
        /// What shape/dtype was found instead.
        detail: String,
    },
}

impl fmt::Display for NpyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Truncated { reading } => write!(f, "truncated .npy while reading {reading}"),
            Self::BadMagic => write!(f, "not a .npy file (bad magic)"),
            Self::UnsupportedVersion { major, minor } => {
                write!(f, "unsupported .npy format version {major}.{minor}")
            }
            Self::HeaderTooLarge { len, max } => {
                write!(f, ".npy header length {len} exceeds cap {max}")
            }
            Self::Header { detail } => write!(f, "malformed .npy header: {detail}"),
            Self::UnsupportedDescr { descr } => write!(
                f,
                "unsupported .npy descr {descr:?}: interchange subset is <f8, <f4, <i8"
            ),
            Self::FortranOrder => {
                write!(
                    f,
                    ".npy declares fortran_order: interchange subset is C order only"
                )
            }
            Self::TooManyDims { dims, max } => {
                write!(f, ".npy declares {dims} dimensions, cap is {max}")
            }
            Self::Overflow => write!(f, ".npy size arithmetic overflowed"),
            Self::DataLength { expected, actual } => write!(
                f,
                ".npy payload holds {actual} elements but the shape declares {expected}"
            ),
            Self::NotPoints { detail } => {
                write!(f, ".npy array is not a (n, 3) <f8 point run: {detail}")
            }
        }
    }
}

impl std::error::Error for NpyError {}

fn element_count(shape: &[usize]) -> Result<usize, NpyError> {
    if shape.is_empty() || shape.len() > MAX_DIMS {
        return Err(NpyError::TooManyDims {
            dims: shape.len(),
            max: MAX_DIMS,
        });
    }
    shape
        .iter()
        .try_fold(1usize, |acc, &d| acc.checked_mul(d))
        .ok_or(NpyError::Overflow)
}

/// Decode a `.npy` document from `bytes`.
///
/// # Errors
/// A precise [`NpyError`] naming the first thing wrong with the document.
pub fn read_npy(bytes: &[u8]) -> Result<NpyArray, NpyError> {
    let magic = bytes
        .first_chunk::<6>()
        .ok_or(NpyError::Truncated { reading: "magic" })?;
    if magic != MAGIC {
        return Err(NpyError::BadMagic);
    }
    let &[major, minor] = bytes
        .get(6..8)
        .ok_or(NpyError::Truncated { reading: "version" })?
    else {
        // get(6..8) yields exactly two bytes; keep the reader panic-free anyway.
        return Err(NpyError::Truncated { reading: "version" });
    };
    let (header_len, header_start): (usize, usize) = match (major, minor) {
        (1, 0) => {
            let len = bytes.get(8..10).ok_or(NpyError::Truncated {
                reading: "header length",
            })?;
            (usize::from(u16::from_le_bytes([len[0], len[1]])), 10)
        }
        (2, 0) => {
            let len = bytes.get(8..12).ok_or(NpyError::Truncated {
                reading: "header length",
            })?;
            let len = u32::from_le_bytes([len[0], len[1], len[2], len[3]]);
            (usize::try_from(len).map_err(|_| NpyError::Overflow)?, 12)
        }
        _ => return Err(NpyError::UnsupportedVersion { major, minor }),
    };
    if header_len > MAX_HEADER_LEN {
        return Err(NpyError::HeaderTooLarge {
            len: header_len,
            max: MAX_HEADER_LEN,
        });
    }
    let header_end = header_start
        .checked_add(header_len)
        .ok_or(NpyError::Overflow)?;
    let header = bytes
        .get(header_start..header_end)
        .ok_or(NpyError::Truncated { reading: "header" })?;
    if !header.is_ascii() {
        return Err(NpyError::Header {
            detail: "header is not ASCII".to_string(),
        });
    }
    let header = std::str::from_utf8(header).map_err(|_| NpyError::Header {
        detail: "header is not ASCII".to_string(),
    })?;
    let (descr, fortran, shape) = parse_header_dict(header)?;
    if fortran {
        return Err(NpyError::FortranOrder);
    }
    let dtype = match descr.as_str() {
        "<f8" => DType::F64,
        "<f4" => DType::F32,
        "<i8" => DType::I64,
        _ => return Err(NpyError::UnsupportedDescr { descr }),
    };
    let count = element_count(&shape)?;
    let payload_len = count.checked_mul(dtype.size()).ok_or(NpyError::Overflow)?;
    let payload = bytes
        .get(header_end..)
        .ok_or(NpyError::Truncated { reading: "payload" })?;
    if payload.len() != payload_len {
        return Err(NpyError::DataLength {
            expected: count,
            actual: payload.len() / dtype.size(),
        });
    }
    let data = match dtype {
        DType::F64 => {
            let (chunks, _rem) = payload.as_chunks::<8>();
            NpyData::F64(chunks.iter().map(|c| f64::from_le_bytes(*c)).collect())
        }
        DType::F32 => {
            let (chunks, _rem) = payload.as_chunks::<4>();
            NpyData::F32(chunks.iter().map(|c| f32::from_le_bytes(*c)).collect())
        }
        DType::I64 => {
            let (chunks, _rem) = payload.as_chunks::<8>();
            NpyData::I64(chunks.iter().map(|c| i64::from_le_bytes(*c)).collect())
        }
    };
    NpyArray::new(shape, data)
}

/// Encode `array` as a `.npy` v1.0 document, byte-compatible with what
/// `np.save` produces for the same array (numpy's key order, spacing, and
/// 64-byte header padding are reproduced exactly, so round-trips through
/// Python tooling are byte-stable).
#[must_use]
pub fn write_npy(array: &NpyArray) -> Vec<u8> {
    let dtype = array.data.dtype();
    let shape_repr = match array.shape.as_slice() {
        [n] => format!("({n},)"),
        dims => {
            let inner: Vec<String> = dims.iter().map(ToString::to_string).collect();
            format!("({})", inner.join(", "))
        }
    };
    let dict = format!(
        "{{'descr': '{}', 'fortran_order': False, 'shape': {}, }}",
        dtype.descr(),
        shape_repr
    );
    // Pad with spaces so magic(6) + version(2) + len(2) + header is a
    // multiple of 64, with the final header byte a newline (numpy's rule).
    let unpadded = 10 + dict.len() + 1;
    let padding = (64 - unpadded % 64) % 64;
    let header_len = dict.len() + padding + 1;
    let mut out = Vec::with_capacity(10 + header_len + array.data.len() * dtype.size());
    out.extend_from_slice(MAGIC);
    out.extend_from_slice(&[1, 0]);
    // header_len is bounded by the dict repr of at most MAX_DIMS dimensions
    // plus 64 bytes of padding (< 300), so the u16 cast cannot truncate.
    #[allow(clippy::cast_possible_truncation)]
    out.extend_from_slice(&(header_len as u16).to_le_bytes());
    out.extend_from_slice(dict.as_bytes());
    out.resize(out.len() + padding, b' ');
    out.push(b'\n');
    match &array.data {
        NpyData::F64(v) => {
            for x in v {
                out.extend_from_slice(&x.to_le_bytes());
            }
        }
        NpyData::F32(v) => {
            for x in v {
                out.extend_from_slice(&x.to_le_bytes());
            }
        }
        NpyData::I64(v) => {
            for x in v {
                out.extend_from_slice(&x.to_le_bytes());
            }
        }
    }
    out
}

/// Parse the header dict: exactly the keys `descr` (string), `fortran_order`
/// (True/False), and `shape` (tuple of non-negative integers), in any order,
/// with unknown keys rejected by name.
fn parse_header_dict(header: &str) -> Result<(String, bool, Vec<usize>), NpyError> {
    let s = header.trim();
    let inner = s
        .strip_prefix('{')
        .and_then(|t| t.strip_suffix('}'))
        .ok_or_else(|| NpyError::Header {
            detail: "header is not a dict literal".to_string(),
        })?;
    let mut descr: Option<String> = None;
    let mut fortran: Option<bool> = None;
    let mut shape: Option<Vec<usize>> = None;
    let mut rest = inner.trim();
    while !rest.is_empty() {
        // Key: a single-quoted identifier.
        let after_quote = rest.strip_prefix('\'').ok_or_else(|| NpyError::Header {
            detail: format!("expected quoted key at {rest:?}"),
        })?;
        let (key, after_key) = after_quote
            .split_once('\'')
            .ok_or_else(|| NpyError::Header {
                detail: "unterminated key quote".to_string(),
            })?;
        let after_colon =
            after_key
                .trim_start()
                .strip_prefix(':')
                .ok_or_else(|| NpyError::Header {
                    detail: format!("expected ':' after key {key:?}"),
                })?;
        let value = after_colon.trim_start();
        rest = match key {
            "descr" => {
                let after = value.strip_prefix('\'').ok_or_else(|| NpyError::Header {
                    detail: "descr is not a string".to_string(),
                })?;
                let (v, tail) = after.split_once('\'').ok_or_else(|| NpyError::Header {
                    detail: "unterminated descr".to_string(),
                })?;
                if descr.replace(v.to_string()).is_some() {
                    return Err(NpyError::Header {
                        detail: "duplicate descr".to_string(),
                    });
                }
                tail
            }
            "fortran_order" => {
                let (v, tail) = if let Some(t) = value.strip_prefix("False") {
                    (false, t)
                } else if let Some(t) = value.strip_prefix("True") {
                    (true, t)
                } else {
                    return Err(NpyError::Header {
                        detail: "fortran_order is not True/False".to_string(),
                    });
                };
                if fortran.replace(v).is_some() {
                    return Err(NpyError::Header {
                        detail: "duplicate fortran_order".to_string(),
                    });
                }
                tail
            }
            "shape" => {
                let after = value.strip_prefix('(').ok_or_else(|| NpyError::Header {
                    detail: "shape is not a tuple".to_string(),
                })?;
                let (tuple, tail) = after.split_once(')').ok_or_else(|| NpyError::Header {
                    detail: "unterminated shape tuple".to_string(),
                })?;
                let mut dims = Vec::new();
                for part in tuple.split(',') {
                    let part = part.trim();
                    if part.is_empty() {
                        continue; // trailing comma in (n,)
                    }
                    dims.push(part.parse::<usize>().map_err(|_| NpyError::Header {
                        detail: format!("bad shape dimension {part:?}"),
                    })?);
                }
                if shape.replace(dims).is_some() {
                    return Err(NpyError::Header {
                        detail: "duplicate shape".to_string(),
                    });
                }
                tail
            }
            other => {
                return Err(NpyError::Header {
                    detail: format!("unknown header key {other:?}"),
                });
            }
        };
        // Consume one separating comma (and surrounding space), if present.
        rest = rest.trim_start();
        if let Some(t) = rest.strip_prefix(',') {
            rest = t.trim_start();
        } else if !rest.is_empty() {
            return Err(NpyError::Header {
                detail: format!("expected ',' between entries at {rest:?}"),
            });
        }
    }
    match (descr, fortran, shape) {
        (Some(d), Some(f), Some(s)) => Ok((d, f, s)),
        (d, f, s) => Err(NpyError::Header {
            detail: format!(
                "missing keys: descr={} fortran_order={} shape={}",
                d.is_some(),
                f.is_some(),
                s.is_some()
            ),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_f64_2d() {
        let a = NpyArray::new(
            vec![2, 3],
            NpyData::F64(vec![0.0, -0.0, 1.5, -2.25, f64::MAX, f64::MIN_POSITIVE]),
        )
        .unwrap();
        let bytes = write_npy(&a);
        // Header block is 64-byte aligned and v1.0.
        assert_eq!(&bytes[..6], MAGIC);
        assert_eq!(&bytes[6..8], &[1, 0]);
        let hlen = usize::from(u16::from_le_bytes([bytes[8], bytes[9]]));
        assert_eq!((10 + hlen) % 64, 0);
        assert_eq!(bytes[10 + hlen - 1], b'\n');
        let b = read_npy(&bytes).unwrap();
        assert_eq!(a, b);
        // −0.0 survives bit-exactly (interchange is bytes, not values).
        assert_eq!(b.as_f64().unwrap()[1].to_bits(), (-0.0f64).to_bits());
    }

    #[test]
    fn round_trip_f32_and_i64_1d() {
        for data in [
            NpyData::F32(vec![1.0, -3.5, f32::EPSILON]),
            NpyData::I64(vec![i64::MIN, -1, 0, i64::MAX]),
        ] {
            let n = data.len();
            let a = NpyArray::new(vec![n], data).unwrap();
            assert_eq!(read_npy(&write_npy(&a)).unwrap(), a);
        }
    }

    #[test]
    fn header_parses_in_any_key_order() {
        let (d, f, s) =
            parse_header_dict("{'shape': (4, 3), 'fortran_order': False, 'descr': '<f4'}").unwrap();
        assert_eq!((d.as_str(), f, s), ("<f4", false, vec![4, 3]));
    }

    #[test]
    fn named_errors_for_bad_documents() {
        assert_eq!(read_npy(b"not npy").unwrap_err(), NpyError::BadMagic);
        let a = NpyArray::new(vec![1], NpyData::F64(vec![1.0])).unwrap();
        let mut bytes = write_npy(&a);
        // Truncate the payload: precise DataLength error.
        bytes.truncate(bytes.len() - 4);
        assert!(matches!(
            read_npy(&bytes).unwrap_err(),
            NpyError::DataLength { expected: 1, .. }
        ));
        // Fortran order is refused by name.
        let hdr = "{'descr': '<f8', 'fortran_order': True, 'shape': (1,)}";
        assert_eq!(
            parse_header_dict(hdr).map(|t| t.1),
            Ok(true),
            "parser reads it; read_npy refuses it"
        );
        // Unknown keys are refused by name.
        assert!(matches!(
            parse_header_dict("{'descr': '<f8', 'fortran_order': False, 'shape': (1,), 'x': 1}"),
            Err(NpyError::Header { .. })
        ));
    }

    #[test]
    fn to_points_requires_n_by_3_f64() {
        let pts = NpyArray::new(vec![2, 3], NpyData::F64((0..6).map(f64::from).collect())).unwrap();
        assert_eq!(
            pts.to_points().unwrap(),
            vec![[0.0, 1.0, 2.0], [3.0, 4.0, 5.0]]
        );
        let flat = NpyArray::new(vec![6], NpyData::F64((0..6).map(f64::from).collect())).unwrap();
        assert!(matches!(flat.to_points(), Err(NpyError::NotPoints { .. })));
    }

    #[test]
    fn shape_payload_disagreement_is_refused() {
        assert!(matches!(
            NpyArray::new(vec![2, 3], NpyData::F64(vec![0.0; 5])),
            Err(NpyError::DataLength {
                expected: 6,
                actual: 5
            })
        ));
    }
}
