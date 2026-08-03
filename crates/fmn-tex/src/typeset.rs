//! The typeset result: fmd-math's layout plus the submobject structure and
//! the span-map consumption surface (§11.4–11.5), serializable for the
//! content-addressed cache.
//!
//! # Submobject structure (the SingleStringTex conventions, span-first)
//!
//! A [`Typeset`] enumerates its primitives as ordered submobjects: every
//! placed glyph (in emission order), then every rule, then every drawn
//! path, each carrying its source span. The *span* is the compatibility
//! surface — `isolate=`, `tex_to_color_map`, substring slicing, and
//! `TransformMatchingTex` all match by source identity through
//! [`Typeset::occurrences`] (§11.3's consumption pattern; the Reference's
//! render-twice-and-align hack is dead). Ordinal positions are stable and
//! deterministic but deliberately **not** promised to match the Reference's
//! SVG-document ordering — index-based poking ports via spans, per the
//! Ledger.
//!
//! # Serialization
//!
//! [`Typeset::to_bytes`]/[`Typeset::from_bytes`] are the cache payload
//! codec: versioned magic, fixed little-endian, length-prefixed, floats as
//! IEEE-754 bits — a cache hit reproduces the layout **bit-for-bit**
//! (tested), which is what lets the cache key participate in the certified
//! input closure: a hit is definitionally equivalent to a recompute.
//! Encoding and decoding are total and allocation-fallible: corrupt,
//! non-canonical, over-limit, or temporarily unallocatable documents return
//! a typed [`TypesetError`] (the cache treats every one as a miss), never a
//! partial payload or a panic.

use fmd_math::{Layout, PathContour, PathSeg, PlacedGlyph, PlacedPath, PlacedRule, Span};
use std::collections::TryReserveError;
use std::fmt;
use std::str::Utf8Error;

/// The serialization format tag; bump on any layout change to the byte
/// format (the cache namespace version rides this).
pub const TYPESET_FORMAT_VERSION: u32 = 1;

/// Maximum encoded FMNTEX document size.
///
/// This matches fmn-hash's canonical field ceiling. A larger in-memory
/// layout remains usable, but deliberately goes uncached.
pub const TYPESET_DOCUMENT_LIMIT_BYTES: usize = 64 * 1024 * 1024;

const MAGIC: &[u8; 8] = b"FMNTEX\x00\x01";
const U32_BYTES: usize = 4;
const F64_BYTES: usize = 8;
const GLYPH_BYTES: usize = 44;
const RULE_BYTES: usize = 40;
const PATH_MIN_BYTES: usize = 12;
const CONTOUR_MIN_BYTES: usize = 20;
const LINE_SEGMENT_BYTES: usize = 17;
const QUAD_SEGMENT_BYTES: usize = 33;

/// Failure to construct, encode, or decode a [`Typeset`] document.
#[derive(Debug)]
pub enum TypesetError {
    /// The encoded document exceeds the format's fixed resource ceiling.
    DocumentTooLarge {
        /// Required or supplied document bytes.
        bytes: usize,
        /// Maximum admitted document bytes.
        limit: usize,
    },
    /// A public count or source offset cannot be represented by the wire's
    /// fixed-width `u32` field.
    IntegerOutOfRange {
        /// Field that cannot be represented.
        field: &'static str,
        /// In-memory value that was refused.
        value: usize,
    },
    /// Aggregate encoded or in-memory size arithmetic overflowed.
    SizeOverflow {
        /// Aggregate whose size could not be represented.
        context: &'static str,
    },
    /// Required owned storage could not be reserved.
    AllocationFailed {
        /// Storage being reserved.
        context: &'static str,
        /// Elements or bytes requested, as named by `context`.
        requested: usize,
        /// Allocator refusal.
        error: TryReserveError,
    },
    /// The format magic/version tag is not FMNTEX v1.
    InvalidMagic,
    /// A fixed or declared field ran past the supplied document.
    UnexpectedEnd {
        /// Bytes requested by the field.
        requested: usize,
        /// Bytes still available.
        remaining: usize,
    },
    /// The source field is not UTF-8.
    InvalidUtf8 {
        /// UTF-8 validation failure.
        error: Utf8Error,
    },
    /// A collection count cannot fit in the bytes still present even at its
    /// smallest legal item encoding.
    ImpossibleCount {
        /// Collection carrying the count.
        field: &'static str,
        /// Declared item count.
        count: usize,
        /// Smallest bytes per item.
        minimum_item_bytes: usize,
        /// Bytes remaining after the count.
        remaining_bytes: usize,
    },
    /// A path segment carries no defined FMNTEX tag.
    InvalidSegmentTag {
        /// Refused tag byte.
        tag: u8,
    },
    /// Bytes remain after the single canonical document.
    TrailingBytes {
        /// Unconsumed byte count.
        bytes: usize,
    },
    /// Public or decoded data has no canonical FMNTEX representation.
    NonCanonical {
        /// Field or table that failed validation.
        field: &'static str,
        /// Stable refusal reason.
        reason: &'static str,
    },
    /// A redundant structural count disagrees with the canonical layout.
    CountMismatch {
        /// Count being checked.
        field: &'static str,
        /// Count implied by the layout.
        expected: usize,
        /// Count carried by the document or public table.
        actual: usize,
    },
}

impl fmt::Display for TypesetError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::DocumentTooLarge { bytes, limit } => {
                write!(f, "FMNTEX document has {bytes} bytes; limit is {limit}")
            }
            Self::IntegerOutOfRange { field, value } => {
                write!(f, "FMNTEX {field} value {value} does not fit u32")
            }
            Self::SizeOverflow { context } => {
                write!(f, "FMNTEX {context} size overflowed usize")
            }
            Self::AllocationFailed {
                context,
                requested,
                error,
            } => write!(
                f,
                "FMNTEX could not reserve {requested} units for {context}: {error}"
            ),
            Self::InvalidMagic => f.write_str("not an FMNTEX v1 document"),
            Self::UnexpectedEnd {
                requested,
                remaining,
            } => write!(
                f,
                "FMNTEX field needs {requested} bytes but only {remaining} remain"
            ),
            Self::InvalidUtf8 { error } => write!(f, "FMNTEX source is not UTF-8: {error}"),
            Self::ImpossibleCount {
                field,
                count,
                minimum_item_bytes,
                remaining_bytes,
            } => write!(
                f,
                "FMNTEX {field} count {count} needs at least {minimum_item_bytes} bytes per item, but only {remaining_bytes} bytes remain"
            ),
            Self::InvalidSegmentTag { tag } => {
                write!(f, "FMNTEX path segment tag {tag} is undefined")
            }
            Self::TrailingBytes { bytes } => {
                write!(f, "FMNTEX document has {bytes} trailing bytes")
            }
            Self::NonCanonical { field, reason } => {
                write!(f, "FMNTEX {field} is non-canonical: {reason}")
            }
            Self::CountMismatch {
                field,
                expected,
                actual,
            } => write!(
                f,
                "FMNTEX {field} count is {actual}; canonical layout requires {expected}"
            ),
        }
    }
}

impl std::error::Error for TypesetError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::AllocationFailed { error, .. } => Some(error),
            Self::InvalidUtf8 { error } => Some(error),
            _ => None,
        }
    }
}

/// Which primitive a submobject is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Prim {
    /// `Layout::glyphs[i]`.
    Glyph(usize),
    /// `Layout::rules[i]`.
    Rule(usize),
    /// `Layout::paths[i]`.
    Path(usize),
}

/// One submobject: a primitive plus its source span.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Sub {
    /// The primitive.
    pub prim: Prim,
    /// Its source byte span.
    pub span: Span,
}

/// A typeset string: the layout, the submobject table, and the source.
#[derive(Clone, Debug, PartialEq)]
pub struct Typeset {
    /// The source string, verbatim.
    pub source: String,
    /// fmd-math's placed output (ems, y-up, baseline 0).
    pub layout: Layout,
    /// The ordered submobjects.
    pub subs: Vec<Sub>,
}

impl Typeset {
    /// Build the submobject table over a layout.
    ///
    /// # Errors
    ///
    /// [`TypesetError::SizeOverflow`] if the primitive counts cannot be
    /// aggregated, or [`TypesetError::AllocationFailed`] if the canonical
    /// submobject table cannot be reserved.
    pub fn new(source: String, layout: Layout) -> Result<Self, TypesetError> {
        let sub_count = primitive_count(&layout)?;
        let mut subs = Vec::new();
        reserve_exact(&mut subs, sub_count, "submobject table")?;
        for (i, g) in layout.glyphs.iter().enumerate() {
            subs.push(Sub {
                prim: Prim::Glyph(i),
                span: g.span,
            });
        }
        for (i, r) in layout.rules.iter().enumerate() {
            subs.push(Sub {
                prim: Prim::Rule(i),
                span: r.span,
            });
        }
        for (i, p) in layout.paths.iter().enumerate() {
            subs.push(Sub {
                prim: Prim::Path(i),
                span: p.span,
            });
        }
        Ok(Self {
            source,
            layout,
            subs,
        })
    }

    pub(crate) fn from_borrowed(source: &str, layout: Layout) -> Result<Self, TypesetError> {
        let mut owned = String::new();
        owned
            .try_reserve_exact(source.len())
            .map_err(|error| TypesetError::AllocationFailed {
                context: "source string",
                requested: source.len(),
                error,
            })?;
        owned.push_str(source);
        Self::new(owned, layout)
    }

    /// The submobject ordinals selected by each occurrence of `needle` in
    /// the source — the `isolate=` / `tex_to_color_map` /
    /// `TransformMatchingTex` surface, by source identity (§11.3).
    #[must_use]
    pub fn occurrences(&self, needle: &str) -> Vec<Vec<usize>> {
        fmd_math::find_occurrences(&self.source, needle)
            .into_iter()
            .map(|span| {
                let sel = self.layout.select(span);
                let mut ords = Vec::new();
                for (ord, sub) in self.subs.iter().enumerate() {
                    let hit = match sub.prim {
                        Prim::Glyph(i) => sel.glyphs.contains(&i),
                        Prim::Rule(i) => sel.rules.contains(&i),
                        Prim::Path(i) => sel.paths.contains(&i),
                    };
                    if hit {
                        ords.push(ord);
                    }
                }
                ords
            })
            .collect()
    }

    // ── The cache payload codec ─────────────────────────────────────────

    /// Serialize for the cache: versioned, little-endian, bit-exact floats.
    #[must_use]
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut w = Wr(Vec::with_capacity(256 + self.source.len()));
        w.0.extend_from_slice(MAGIC);
        w.bytes(self.source.as_bytes());
        w.f64(self.layout.width);
        w.f64(self.layout.height);
        w.f64(self.layout.depth);
        w.u32(self.layout.glyphs.len());
        for g in &self.layout.glyphs {
            w.u32(g.face.0);
            w.u32(usize::from(g.gid));
            w.u32(g.ch as usize);
            w.f64(g.x);
            w.f64(g.y);
            w.f64(g.size);
            w.span(g.span);
        }
        w.u32(self.layout.rules.len());
        for r in &self.layout.rules {
            w.f64(r.x);
            w.f64(r.y);
            w.f64(r.width);
            w.f64(r.height);
            w.span(r.span);
        }
        w.u32(self.layout.paths.len());
        for p in &self.layout.paths {
            w.span(p.span);
            w.u32(p.contours.len());
            for c in &p.contours {
                w.f64(c.start.0);
                w.f64(c.start.1);
                w.u32(c.segments.len());
                for s in &c.segments {
                    match s {
                        PathSeg::Line { to } => {
                            w.0.push(1);
                            w.f64(to.0);
                            w.f64(to.1);
                        }
                        PathSeg::Quad { ctrl, to } => {
                            w.0.push(2);
                            w.f64(ctrl.0);
                            w.f64(ctrl.1);
                            w.f64(to.0);
                            w.f64(to.1);
                        }
                    }
                }
            }
        }
        // The submobject table is derivable from the layout; store only a
        // count for a structural cross-check on decode.
        w.u32(self.subs.len());
        w.0
    }

    /// Decode a cache payload. `None` on any structural fault — the caller
    /// treats it as a miss and re-typesets (never trusted, never fatal).
    #[must_use]
    pub fn from_bytes(bytes: &[u8]) -> Option<Self> {
        let mut r = Rd { b: bytes, at: 0 };
        if r.take(MAGIC.len())? != MAGIC.as_slice() {
            return None;
        }
        let source = String::from_utf8(r.bytes()?.to_vec()).ok()?;
        let width = r.f64()?;
        let height = r.f64()?;
        let depth = r.f64()?;
        let mut layout = Layout {
            width,
            height,
            depth,
            ..Layout::default()
        };
        for _ in 0..r.u32()? {
            layout.glyphs.push(PlacedGlyph {
                face: fmd_math::FaceId(r.u32()?),
                gid: u16::try_from(r.u32()?).ok()?,
                ch: char::from_u32(u32::try_from(r.u32()?).ok()?)?,
                x: r.f64()?,
                y: r.f64()?,
                size: r.f64()?,
                span: r.span()?,
            });
        }
        for _ in 0..r.u32()? {
            layout.rules.push(PlacedRule {
                x: r.f64()?,
                y: r.f64()?,
                width: r.f64()?,
                height: r.f64()?,
                span: r.span()?,
            });
        }
        for _ in 0..r.u32()? {
            let span = r.span()?;
            let mut contours = Vec::new();
            for _ in 0..r.u32()? {
                let start = (r.f64()?, r.f64()?);
                let mut segments = Vec::new();
                for _ in 0..r.u32()? {
                    let tag = *r.take(1)?.first()?;
                    segments.push(match tag {
                        1 => PathSeg::Line {
                            to: (r.f64()?, r.f64()?),
                        },
                        2 => PathSeg::Quad {
                            ctrl: (r.f64()?, r.f64()?),
                            to: (r.f64()?, r.f64()?),
                        },
                        _ => return None,
                    });
                }
                contours.push(PathContour { start, segments });
            }
            layout.paths.push(PlacedPath { contours, span });
        }
        let expected_subs = r.u32()?;
        if r.at != bytes.len() {
            return None; // trailing garbage
        }
        let typeset = Self::new(source, layout);
        if typeset.subs.len() != expected_subs {
            return None;
        }
        Some(typeset)
    }
}

/// Little-endian writer.
struct Wr(Vec<u8>);

impl Wr {
    fn u32(&mut self, v: usize) {
        // Counts in a typeset layout are far below u32::MAX; saturate
        // defensively rather than truncate.
        let v = u32::try_from(v).unwrap_or(u32::MAX);
        self.0.extend_from_slice(&v.to_le_bytes());
    }
    fn f64(&mut self, v: f64) {
        self.0.extend_from_slice(&v.to_bits().to_le_bytes());
    }
    fn span(&mut self, s: Span) {
        self.u32(s.start);
        self.u32(s.end);
    }
    fn bytes(&mut self, b: &[u8]) {
        self.u32(b.len());
        self.0.extend_from_slice(b);
    }
}

/// Bounds-checked little-endian reader; every method is total.
struct Rd<'a> {
    b: &'a [u8],
    at: usize,
}

impl<'a> Rd<'a> {
    fn take(&mut self, n: usize) -> Option<&'a [u8]> {
        let end = self.at.checked_add(n)?;
        if end > self.b.len() {
            return None;
        }
        let s = &self.b[self.at..end];
        self.at = end;
        Some(s)
    }
    fn u32(&mut self) -> Option<usize> {
        let s = self.take(4)?;
        Some(u32::from_le_bytes([s[0], s[1], s[2], s[3]]) as usize)
    }
    fn f64(&mut self) -> Option<f64> {
        let s = self.take(8)?;
        Some(f64::from_bits(u64::from_le_bytes([
            s[0], s[1], s[2], s[3], s[4], s[5], s[6], s[7],
        ])))
    }
    fn span(&mut self) -> Option<Span> {
        let start = self.u32()?;
        let end = self.u32()?;
        Some(Span::new(start, end))
    }
    fn bytes(&mut self) -> Option<&'a [u8]> {
        let n = self.u32()?;
        self.take(n)
    }
}
