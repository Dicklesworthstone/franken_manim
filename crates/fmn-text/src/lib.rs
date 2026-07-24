//! Scribe I: native text shaping, markup, and layout over fmd-font
//! (§11.2), with §11.3 span provenance on every glyph.
//!
//! The pipeline: source text → styled characters (plain, or the manim
//! markup tag set with precise line:column diagnostics) → the
//! `t2c`/`t2f`/`t2g`/`t2s`/`t2w` maps applied by source-byte occurrence →
//! shaping (cmap→gids, kern + focused-GPOS kerning, bundled ligature sets
//! **off by default** to keep the familiar manim look) → line breaking
//! (greedy with manim's width semantics; least-badness as an explicit
//! option) → [`TextLayout`]: positioned glyphs in ems (y-up, first
//! baseline at 0), each carrying its face, source span, character index,
//! and submobject ordinal — the `Text[3:7]` / `isolate=` compatibility
//! surface is structural, exactly the Reference's `StringMobject`
//! conventions (non-whitespace glyphs, in order; a ligature is one
//! submobject covering its character range).
//!
//! **Font policy (D-08).** The bundled default face renders identically on
//! every machine; user TTFs load from bytes; family-name lookup never
//! silently substitutes — a miss is a named capability-style error. The
//! default text face is bundled Computer Modern where the Reference
//! defaulted to a host font through Pango; metric differences are
//! Behavior-Noted (BN-05).
//!
//! Output geometry rides the proven fmd-font→QuadPath transcription seam:
//! TrueType outlines are already quadratic, so [`glyph_quadpath`] is
//! transcription, not approximation.

#![forbid(unsafe_code)]

pub mod error;
pub mod font;
pub mod layout;
pub mod maps;
pub mod markup;
pub mod shape;

pub use error::TextError;
pub use font::{DEFAULT_FAMILY, FontBook, MONO_FAMILY, SANS_FAMILY};
pub use layout::{
    Align, Decoration, Line, LineBreaker, PlacedTextGlyph, TextLayout, TextRequest, layout_text,
};
pub use maps::StyleMaps;

use fmn_core::types::Vec3;
use fmn_geom::QuadPath;

/// The outline of a placed glyph as a positioned [`QuadPath`], in the
/// layout's em coordinates: one subpath per contour, scaled by the
/// glyph's size and translated to its position — 1:1 transcription of the
/// decoded quadratic segments.
///
/// # Errors
///
/// [`TextError::FontUnavailable`] if the glyph's family left the book;
/// [`TextError::Outline`] on a decode failure.
pub fn glyph_quadpath(book: &FontBook, glyph: &PlacedTextGlyph) -> Result<QuadPath, TextError> {
    let family = book.family(&glyph.face.family)?;
    let font = &family.face(glyph.face.key).font;
    let outline = font
        .glyph_outline(glyph.gid)
        .map_err(|e| TextError::Outline {
            ch: glyph.ch,
            what: format!("{e:?}"),
        })?;
    let upm = f64::from(font.units_per_em.max(1));
    let s = glyph.size / upm;
    let v = |x: f64, y: f64| -> Vec3 { [glyph.x + x * s, glyph.y + y * s, 0.0] };
    let mut path = QuadPath::new();
    for contour in &outline.contours {
        path.start_new_path(v(contour.start.x, contour.start.y));
        for seg in &contour.segments {
            match seg {
                fmd_font::outline::Segment::Line { to } => {
                    path.add_line_to(v(to.x, to.y), true)
                        .map_err(|e| TextError::Outline {
                            ch: glyph.ch,
                            what: format!("{e:?}"),
                        })?;
                }
                fmd_font::outline::Segment::Quad { ctrl, to } => {
                    path.add_quadratic_bezier_curve_to(v(ctrl.x, ctrl.y), v(to.x, to.y), true)
                        .map_err(|e| TextError::Outline {
                            ch: glyph.ch,
                            what: format!("{e:?}"),
                        })?;
                }
            }
        }
    }
    Ok(path)
}
