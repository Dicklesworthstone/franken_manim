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

/// The five maps, all keyed by source substring.
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

/// Apply the maps to styled characters in place.
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
            let covered_ix: Vec<usize> = chars
                .iter()
                .enumerate()
                .filter(|(_, c)| c.span.0 >= occ.0 && c.span.1 <= occ.1)
                .map(|(i, _)| i)
                .collect();
            let denom = covered_ix.len().saturating_sub(1).max(1) as f64;
            for (pos, ix) in covered_ix.into_iter().enumerate() {
                let t = pos as f64 / denom;
                chars[ix].style.color = None;
                chars[ix].style.gradient = Some((stops.to_vec(), t));
            }
        }
    }
}

fn covered(chars: &mut [StyledChar], occ: (usize, usize)) -> impl Iterator<Item = &mut StyledChar> {
    chars
        .iter_mut()
        .filter(move |c| c.span.0 >= occ.0 && c.span.1 <= occ.1)
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
