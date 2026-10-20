//! The `t2c`/`t2f`/`t2g`/`t2s`/`t2w` maps (§11.2): substring→style
//! overrides applied by **source-byte occurrence** — the same
//! source-identity discipline as the math span map (W6SPAN), no second
//! render, no alignment.
//!
//! A map entry applies to every character whose span is contained in an
//! occurrence of the key. Later map kinds never clobber unrelated fields;
//! within one kind, later entries win (map-literal order), matching the
//! Reference's dict-update behavior.

use crate::markup::StyledChar;
use fmn_core::color::Srgb;
use std::borrow::Cow;

/// A literal source key or already-resolved UTF-8 source ranges.
///
/// Regex and character-index selectors are resolved by the authoring front
/// door. The layouter receives exact source bytes, never rewritten markup.
#[derive(Clone, Copy, Debug)]
pub enum SourceSelector<'a> {
    /// Every non-overlapping occurrence of this source string.
    Literal(&'a str),
    /// Selected source occurrences, in authored order.
    Ranges(&'a [(usize, usize)]),
}

impl SourceSelector<'_> {
    fn ranges(&self, source: &str) -> Cow<'_, [(usize, usize)]> {
        match self {
            Self::Literal(key) => Cow::Owned(find_occurrences(source, key)),
            Self::Ranges(ranges) => Cow::Borrowed(*ranges),
        }
    }
}

/// An ordered style update over a source selector, before shaping.
///
/// Only supplied fields change. Later entries win on their own fields, so
/// overlapping font, weight, slant and gradient maps compose. Each occurrence
/// receives its own gradient sweep.
#[derive(Clone, Debug)]
pub struct SourceStyle<'a> {
    /// Which source characters receive the style.
    pub selector: SourceSelector<'a>,
    /// Fill color, if supplied.
    pub color: Option<Srgb>,
    /// Font family, if supplied.
    pub family: Option<&'a str>,
    /// Italic face override, including an explicit normal face.
    pub italic: Option<bool>,
    /// Bold face override, including an explicit normal face.
    pub bold: Option<bool>,
    /// Gradient stops; empty preserves the existing paint.
    pub gradient: &'a [Srgb],
}

impl<'a> SourceStyle<'a> {
    /// A selector with no style fields overridden yet.
    #[must_use]
    pub fn new(selector: SourceSelector<'a>) -> Self {
        Self {
            selector,
            color: None,
            family: None,
            italic: None,
            bold: None,
            gradient: &[],
        }
    }
}

/// The five literal maps and ordered source-selector styles.
#[derive(Clone, Debug, Default)]
pub struct StyleMaps<'a> {
    /// text→color.
    pub t2c: &'a [(&'a str, Srgb)],
    /// text→font family (resolved against the book at shaping; a miss is
    /// the named capability error).
    pub t2f: &'a [(&'a str, &'a str)],
    /// text→gradient stops.
    pub t2g: &'a [(&'a str, &'a [Srgb])],
    /// text→slant (true = italic).
    pub t2s: &'a [(&'a str, bool)],
    /// text→weight (true = bold).
    pub t2w: &'a [(&'a str, bool)],
    /// Ordered source-selector styles, after literal maps and before Code's
    /// positional highlighter channel.
    pub source_styles: &'a [SourceStyle<'a>],
}

impl StyleMaps<'_> {
    /// True when every map is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.t2c.is_empty()
            && self.t2f.is_empty()
            && self.t2g.is_empty()
            && self.t2s.is_empty()
            && self.t2w.is_empty()
            && self.source_styles.is_empty()
    }
}

/// Byte occurrences of `needle` in `source`, non-overlapping, left to
/// right.
#[must_use]
pub fn find_occurrences(source: &str, needle: &str) -> Vec<(usize, usize)> {
    if needle.is_empty() {
        return Vec::new();
    }
    source
        .match_indices(needle)
        .map(|(i, m)| (i, i + m.len()))
        .collect()
}

/// Apply the maps to styled characters in place, in the increasing source-span
/// order emitted by the plain-text and markup parsers before shaping.
pub fn apply_maps(chars: &mut [StyledChar], source: &str, maps: &StyleMaps<'_>) {
    for (key, color) in maps.t2c {
        for occ in find_occurrences(source, key) {
            for c in covered(chars, occ) {
                c.style.color = Some(*color);
                c.style.gradient = None;
            }
        }
    }
    for (key, family) in maps.t2f {
        for occ in find_occurrences(source, key) {
            for c in covered(chars, occ) {
                c.style.family = Some((*family).to_owned());
            }
        }
    }
    for (key, italic) in maps.t2s {
        for occ in find_occurrences(source, key) {
            for c in covered(chars, occ) {
                c.style.italic = *italic;
            }
        }
    }
    for (key, bold) in maps.t2w {
        for occ in find_occurrences(source, key) {
            for c in covered(chars, occ) {
                c.style.bold = *bold;
            }
        }
    }
    for (key, stops) in maps.t2g {
        if stops.is_empty() {
            continue;
        }
        for occ in find_occurrences(source, key) {
            // Position each covered character across the occurrence so the
            // gradient sweeps it left to right.
            let selected = covered(chars, occ);
            let denom = selected.len().saturating_sub(1).max(1) as f64;
            for (pos, character) in selected.iter_mut().enumerate() {
                let t = pos as f64 / denom;
                character.style.color = None;
                character.style.gradient = Some((stops.to_vec(), t));
            }
        }
    }
    for update in maps.source_styles {
        for &range in update.selector.ranges(source).iter() {
            let selected = covered(chars, range);
            let denominator = selected.len().saturating_sub(1).max(1) as f64;
            for (position, character) in selected.iter_mut().enumerate() {
                let style = &mut character.style;
                if let Some(color) = update.color {
                    style.color = Some(color);
                    style.gradient = None;
                }
                if let Some(family) = update.family {
                    style.family = Some(family.to_owned());
                }
                if let Some(italic) = update.italic {
                    style.italic = italic;
                }
                if let Some(bold) = update.bold {
                    style.bold = bold;
                }
                if !update.gradient.is_empty() {
                    style.color = None;
                    style.gradient =
                        Some((update.gradient.to_vec(), position as f64 / denominator));
                }
            }
        }
    }
}

fn covered(chars: &mut [StyledChar], occ: (usize, usize)) -> &mut [StyledChar] {
    // Styled characters remain in source order until shaping. Select the
    // contained run without scanning the full text for every occurrence.
    // A markup entity is selected only when its whole source span fits.
    let start = chars.partition_point(|character| character.span.0 < occ.0);
    let end = chars.partition_point(|character| character.span.1 <= occ.1);
    &mut chars[start..end.max(start)]
}

/// Sample a gradient's stops at `t ∈ [0, 1]` (piecewise-linear in sRGB
/// components, matching the Reference's per-glyph gradient sweep).
#[must_use]
pub fn sample_gradient(stops: &[Srgb], t: f64) -> Srgb {
    match stops {
        [] => Srgb {
            r: 1.0,
            g: 1.0,
            b: 1.0,
        },
        [only] => *only,
        _ => {
            let t = t.clamp(0.0, 1.0) * (stops.len() - 1) as f64;
            let ix = (t.floor() as usize).min(stops.len() - 2);
            let f = t - ix as f64;
            let (a, b) = (stops[ix], stops[ix + 1]);
            Srgb {
                r: a.r + (b.r - a.r) * f,
                g: a.g + (b.g - a.g) * f,
                b: a.b + (b.b - a.b) * f,
            }
        }
    }
}

/// A positional per-character override (fm-u8y): `Some` fields win over
/// everything the maps produced. Index-aligned with the source's chars.
#[derive(Clone, Copy, Debug, Default)]
pub struct CharOverride<'a> {
    /// Fill colour for the character.
    pub color: Option<Srgb>,
    /// Font family for the character (the Code mobject's typewriter).
    pub family: Option<&'a str>,
}

/// The positional channel: one entry per source char, empty to disable.
pub type CharOverrides<'a> = &'a [CharOverride<'a>];

/// Apply the positional overrides in place. Byte-offset bookkeeping is the
/// caller's (`Code` translates fmd's byte spans before calling); here an
/// override at char *i* touches exactly char *i*, so a keyword inside an
/// identifier can never bleed. Chars past the slice keep their mapped style.
pub fn apply_overrides(chars: &mut [StyledChar], overrides: CharOverrides<'_>) {
    if overrides.is_empty() {
        return;
    }
    for (c, over) in chars.iter_mut().zip(overrides) {
        if let Some(color) = over.color {
            c.style.color = Some(color);
            c.style.gradient = None;
        }
        if let Some(family) = over.family {
            c.style.family = Some(family.to_owned());
        }
    }
}

#[cfg(test)]
mod override_tests {
    use super::*;
    use crate::markup::plain_chars;

    fn styled(source: &str, overrides: CharOverrides<'_>) -> Vec<Srgb> {
        let mut chars = plain_chars(source);
        apply_maps(&mut chars, source, &StyleMaps::default());
        apply_overrides(&mut chars, overrides);
        chars
            .into_iter()
            .map(|c| c.style.color.unwrap_or(Srgb::from_rgb8(255, 255, 255)))
            .collect()
    }

    #[test]
    fn positional_overrides_never_bleed_into_identifiers() {
        // The t2c failure mode: colouring "in" would also colour the "in"
        // inside "int". Positional overrides touch exact indices only.
        let source = "int in";
        let mut over: Vec<CharOverride<'_>> =
            source.chars().map(|_| CharOverride::default()).collect();
        // Colour ONLY the standalone "in" (chars 4..6): the same substring
        // inside "int" must stay untouched.
        for slot in &mut over[4..6] {
            slot.color = Some(Srgb::from_rgb8(200, 0, 0));
        }
        let colors = styled(source, &over);
        assert!(
            colors[..4]
                .iter()
                .all(|c| *c == Srgb::from_rgb8(255, 255, 255))
        );
        assert!(
            colors[4..6]
                .iter()
                .all(|c| *c == Srgb::from_rgb8(200, 0, 0))
        );
    }

    #[test]
    fn family_override_lands_on_the_named_chars() {
        let source = "ab";
        let mut over: Vec<CharOverride<'_>> =
            source.chars().map(|_| CharOverride::default()).collect();
        over[1].family = Some("CM Typewriter");
        let mut chars = plain_chars(source);
        apply_overrides(&mut chars, &over);
        assert!(chars[0].style.family.is_none());
        assert_eq!(chars[1].style.family.as_deref(), Some("CM Typewriter"));
    }
}
