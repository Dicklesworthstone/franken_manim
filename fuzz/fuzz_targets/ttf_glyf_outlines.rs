//! fm-fmd-math-fuzz-budgets-k8fe fuzz target: fmd-font past the table
//! directory — every glyph's `glyf` outline (composites included), the
//! `gvar` instancer, and the GSUB/GPOS decoders a renderer reaches.
//!
//! Resource-budget contract (§16.5/R14): for ANY font bytes, each decode
//! either refuses with a typed error or succeeds within the decoder budgets
//! (`MAX_OUTLINE_POINTS` points per glyph, finite coordinates) — never a
//! panic, a hang, or an unbounded allocation.
//!
//! Input: a first byte below 0x40 means the rest is a whole font. Otherwise
//! the rest is a list of byte edits on one of the valid base fonts below,
//! so the search starts inside well-formed `glyf`, composite and `gvar`
//! data instead of having to discover the sfnt structure. Each edit is
//! `[offset: 3 bytes, big-endian, modulo the base length][len: 1 byte]` then
//! `len` replacement bytes; `len == 0` truncates the font at `offset`.
#![no_main]

use std::sync::LazyLock;

use fmd_font::Font;
use fmd_font::outline::{GlyphOutline, MAX_OUTLINE_POINTS, Segment};
use libfuzzer_sys::fuzz_target;

/// Valid base fonts: fmd-font's own variable fixture (`gvar`), and small
/// subsets of the bundled OFL faces taken at run time. Accented Latin
/// capitals are composites in Computer Modern; the Plex subset keeps kerning
/// pairs; the Noto subset has the big operators.
static BASES: LazyLock<Vec<Vec<u8>>> = LazyLock::new(|| {
    let subset = |bytes: &[u8], chars: &str| {
        let font = Font::parse(bytes.to_vec()).expect("bundled face parses");
        let keep: Vec<char> = chars.chars().collect();
        font.subset(&keep).expect("bundled face subsets")
    };
    vec![
        fmd_font::variable_triangle_fixture(),
        subset(fmd_font::bundled::CM_REGULAR, "AÉÅéfiﬁ"),
        subset(fmd_font::bundled::PLEX_REGULAR, "AVTo.,"),
        subset(fmd_font::bundled::NOTO_SANS_MATH_SYMBOLS, "∑∫√"),
        subset(fmd_font::bundled::CM_MATH_EXTENSION_10, "()"),
    ]
});

/// The font bytes an input stands for.
fn font_bytes(data: &[u8]) -> Option<Vec<u8>> {
    let (&mode, rest) = data.split_first()?;
    if mode < 0x40 {
        return Some(rest.to_vec());
    }
    let mut font = BASES[usize::from(mode) % BASES.len()].clone();
    let mut edits = rest;
    while let [a, b, c, len, tail @ ..] = edits {
        if font.is_empty() {
            break;
        }
        let offset = (usize::from(*a) << 16 | usize::from(*b) << 8 | usize::from(*c)) % font.len();
        let len = usize::from(*len);
        if len == 0 {
            font.truncate(offset);
            edits = tail;
            continue;
        }
        let take = len.min(tail.len());
        let end = (offset + take).min(font.len());
        font[offset..end].copy_from_slice(&tail[..end - offset]);
        edits = &tail[take..];
    }
    Some(font)
}

fn check_outline(outline: &GlyphOutline) {
    let mut points = 0_usize;
    for contour in &outline.contours {
        let mut check = |x: f64, y: f64| {
            assert!(x.is_finite() && y.is_finite(), "non-finite outline point");
            points += 1;
        };
        check(contour.start.x, contour.start.y);
        for segment in &contour.segments {
            if let Segment::Quad { ctrl, .. } = segment {
                check(ctrl.x, ctrl.y);
            }
            let to = segment.to();
            check(to.x, to.y);
        }
    }
    // A decoded point becomes at most an emitted point plus an implied
    // on-curve midpoint, and each contour adds its closing point, so a
    // glyph inside the decode budget emits at most three times it.
    assert!(
        points <= 3 * MAX_OUTLINE_POINTS,
        "{points} outline points from one glyph"
    );
}

fn decode_everything(font: &Font) {
    for gid in 0..font.num_glyphs {
        if let Ok(outline) = font.glyph_outline(gid) {
            check_outline(&outline);
        }
        let _ = font.glyph_components(gid);
        let _ = font.advance_width(gid);
    }
    let gsub = font.gsub_ligatures();
    let _ = gsub.substitute(&[0, 1, 2, 3]);
    let gpos = font.gpos_kerning();
    let _ = gpos.pair(0, 1);
    let _ = font.kerning_between_glyphs(0, 1);
}

fuzz_target!(|data: &[u8]| {
    let Some(bytes) = font_bytes(data) else {
        return;
    };
    let Ok(font) = Font::parse(bytes) else {
        return;
    };
    decode_everything(&font);
    // The gvar instancer at the axis ends and the middle; an instance is a
    // static font whose outlines decode under the same budgets.
    if let Some(bounds) = font.instance_bounds(*b"wght") {
        for weight in [bounds.min, bounds.default, bounds.max] {
            if let Some(instance) = font.instance(weight) {
                decode_everything(&instance);
            }
        }
    }
});
