//! The `SVGMobject` consumer over Chisel's hardened user-SVG document
//! processor (fm-5wq.4.50): [`fmn_geom::svg`] parses and resolves the
//! document — budgets, the accept/reject matrix, the full transform cascade
//! — and this module builds the [`VMobject`] family from the resolved
//! records, exactly the seam the processor's docs reserve for fmn-library.
//!
//! The family shape is the Reference's (`svg_mobject.py`): one child per
//! rendered shape, in document order, each carrying its resolved SVG style;
//! shapes that resolved to no geometry are skipped, as the Reference skips
//! pointless mobjects. Geometry stays in the document's viewport user-space
//! coordinates (y-down) — the caller applies the Reference's y-flip,
//! centring, and height normalization, which are mobject-level choices, not
//! document facts.
//!
//! Style mapping follows the Reference's `apply_style_to_mobject`: SVG
//! `stroke-width` passes through as manim stroke-width units unconverted
//! (the Reference feeds `shape.stroke_width` straight into `set_stroke`),
//! and element `opacity` — already multiplied down the group cascade by the
//! processor — folds into both channel opacities, the same flattening the
//! processor documents. `fill-rule` is carried by the geometry's records
//! downstream; a paint of SVG `none` becomes a zero-opacity channel.

use fmn_geom::svg::{Paint, SvgShape, SvgStyle};
// Consumers above this crate (the fmn-python portal) reach the processor's
// document, budgets, and typed refusals through this seam rather than
// depending on the geometry kernel directly.
pub use fmn_geom::svg::{SvgDocument, SvgError, SvgLimits};

use crate::style::Style;
use crate::vmobject::{VMobject, v_group};

/// Build the detached `SVGMobject` family from a resolved document: one
/// styled child per pointful shape, in document order, viewport user-space
/// coordinates.
#[must_use]
pub fn svg_document_mobject(document: &SvgDocument) -> VMobject {
    v_group(
        document
            .shapes
            .iter()
            .filter(|shape| shape.path.has_points())
            .map(shape_child),
    )
}

/// Parse SVG bytes and build the detached `SVGMobject` family in one call,
/// using the default budgets (the untrusted-input path is
/// [`SvgDocument::parse_with_limits`] + [`svg_document_mobject`]).
///
/// # Errors
/// Returns the same typed refusals as [`SvgDocument::parse`].
pub fn svg_mobject(bytes: &[u8]) -> Result<VMobject, SvgError> {
    let document = SvgDocument::parse(bytes)?;
    Ok(svg_document_mobject(&document))
}

/// One shape's child: its shared-anchor point run under its resolved style.
fn shape_child(shape: &SvgShape) -> VMobject {
    VMobject::from_points(shape.path.points().to_vec()).with_style(shape_style(&shape.style))
}

/// The Reference's style mapping for one resolved SVG style record.
fn shape_style(svg: &SvgStyle) -> Style {
    let mut style = Style::default();
    match svg.fill {
        Some(Paint::Color(color)) => {
            style.fill_color = color;
            style.fill_opacity = svg.fill_opacity * svg.opacity;
        }
        None => style.fill_opacity = 0.0,
    }
    style.stroke_width = svg.stroke_width;
    match svg.stroke {
        Some(Paint::Color(color)) => {
            style.stroke_color = color;
            style.stroke_opacity = svg.stroke_opacity * svg.opacity;
        }
        None => style.stroke_opacity = 0.0,
    }
    style
}
