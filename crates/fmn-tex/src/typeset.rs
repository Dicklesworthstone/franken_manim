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
//! Decoding is total: corrupt bytes return `None` (the cache treats that
//! as a miss), never a panic.

use fmd_math::{Layout, PathContour, PathSeg, PlacedGlyph, PlacedPath, PlacedRule, Span};

/// The serialization format tag; bump on any layout change to the byte
/// format (the cache namespace version rides this).
pub const TYPESET_FORMAT_VERSION: u32 = 1;

const MAGIC: &[u8; 8] = b"FMNTEX\x00\x01";

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
    #[must_use]
    pub fn new(source: String, layout: Layout) -> Self {
        let mut subs =
            Vec::with_capacity(layout.glyphs.len() + layout.rules.len() + layout.paths.len());
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
        Self {
            source,
            layout,
            subs,
        }
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
