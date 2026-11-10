//! The manim markup tag set (§11.2): `<b> <i> <u> <s> <tt> <big> <small>
//! <sub> <sup>` and `<span>` with color/weight/style attributes, over
//! XML-ish entities — parsed to styled characters with exact per-character
//! source spans.
//!
//! The parser is untrusted-adjacent: nesting depth, attribute size, and
//! entity length are bounded; every failure is a precise line:column
//! diagnostic; arbitrary input never panics (chaos-tested).

use crate::error::TextError;
use crate::maps::{
    MAX_SCOPED_STYLE_LAYERS, MAX_SCOPED_STYLE_WORK, ScopedStyle, SizeChange, StyleFields,
    style_error, validate_inherited_style, validate_scopes,
};
use fmn_core::color::Srgb;
use std::collections::BTreeSet;

/// Nesting bound for tags.
pub const MAX_TAG_DEPTH: usize = 32;
/// Longest accepted attribute value, bytes.
pub const MAX_ATTR_LEN: usize = 256;

/// Sub/superscript state.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Script {
    /// On the baseline.
    #[default]
    Normal,
    /// `<sub>`
    Sub,
    /// `<sup>`
    Sup,
}

/// The resolved style of one character.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct CharStyle {
    /// `<b>` / `weight="bold"` / `t2w`.
    pub bold: bool,
    /// `<i>` / `style="italic"` / `t2s`.
    pub italic: bool,
    /// `<u>` / `underline="single"`.
    pub underline: bool,
    /// `<s>` strikethrough.
    pub strike: bool,
    /// `<tt>` — the monospace family.
    pub mono: bool,
    /// Sub/superscript state (`<sub>`/`<sup>`).
    pub script: Script,
    /// Cumulative size factor (`<big>` ×1.2, `<small>` ×⅚, scripts ×0.65).
    pub size_factor: f64,
    /// Fill color (span `foreground`, `t2c`).
    pub color: Option<Srgb>,
    /// Gradient fill: stops plus this character's position in its matched
    /// range, 0..=1 (`t2g`).
    pub gradient: Option<(Vec<Srgb>, f64)>,
    /// An explicit family request (span `font_family`, `t2f`) — resolved
    /// against the book at shaping time, where a miss is the named
    /// capability error.
    pub family: Option<String>,
}

impl CharStyle {
    /// An unstyled character at the base font size.
    #[must_use]
    pub fn base() -> Self {
        Self {
            size_factor: 1.0,
            ..Self::default()
        }
    }
}

/// One source character with its byte span and resolved style.
#[derive(Clone, Debug, PartialEq)]
pub struct StyledChar {
    /// The character (entity-decoded).
    pub ch: char,
    /// Byte span in the source (an entity covers its whole `&…;`).
    pub span: (usize, usize),
    /// Index of this character in the decoded character sequence.
    pub char_index: usize,
    /// The style in effect.
    pub style: CharStyle,
}

/// Plain text: every character styled with the base style, spans exact.
/// No tags, no entities — what manim's `Text` does.
#[must_use]
pub fn plain_chars(source: &str) -> Vec<StyledChar> {
    plain_chars_with_style(source, &CharStyle::base())
}

/// Decode plain text with an explicit inherited base style, preserving source spans.
#[must_use]
pub fn plain_chars_with_style(source: &str, base: &CharStyle) -> Vec<StyledChar> {
    source
        .char_indices()
        .enumerate()
        .map(|(char_index, (start, ch))| StyledChar {
            ch,
            span: (start, start + ch.len_utf8()),
            char_index,
            style: base.clone(),
        })
        .collect()
}

/// Decode plain text with inherited and scoped styles over original bytes.
///
/// # Errors
/// Rejects malformed UTF-8 ranges, invalid fields or composed sizes, and
/// requests exceeding the scoped-style layer/work bounds.
pub fn plain_chars_with_scoped_styles(
    source: &str,
    base: &CharStyle,
    scopes: &[ScopedStyle<'_>],
) -> Result<Vec<StyledChar>, TextError> {
    validate_inherited_style(base)?;
    validate_scopes(source, scopes)?;
    let mut chars = plain_chars_with_style(source, base);
    if !scopes.is_empty() {
        apply_scopes(&mut chars, base, &configured_layers(scopes))?;
    }
    Ok(chars)
}

/// Parse the markup dialect to styled characters (what manim's
/// `MarkupText` does).
///
/// # Errors
///
/// [`TextError::Markup`] with a one-based line:column diagnostic for every
/// malformation: unknown tags or attributes, mismatched closers, unclosed
/// tags, oversized attributes, bad entities, depth beyond
/// [`MAX_TAG_DEPTH`].
pub fn parse_markup(source: &str) -> Result<Vec<StyledChar>, TextError> {
    parse_markup_with_style(source, &CharStyle::base())
}

/// Parse markup over an inherited style without inserting synthetic source bytes.
/// Inner tags override the inherited style and closing tags restore it.
///
/// # Errors
/// The same bounded markup diagnostics as [`parse_markup`].
pub fn parse_markup_with_style(
    source: &str,
    base: &CharStyle,
) -> Result<Vec<StyledChar>, TextError> {
    parse_markup_with_scoped_styles(source, base, &[])
}

/// Parse markup with source scopes interleaved by element containment.
///
/// No synthetic tags are inserted: every decoded character and entity keeps
/// its original source span. A scope around a complete element is inherited
/// by that element; a scope within its text can override the inline style.
///
/// # Errors
/// The markup diagnostics of [`parse_markup`], plus invalid configured fields,
/// UTF-8 ranges, size composition, and bounded scope-work errors.
pub fn parse_markup_with_scoped_styles(
    source: &str,
    base: &CharStyle,
    scopes: &[ScopedStyle<'_>],
) -> Result<Vec<StyledChar>, TextError> {
    validate_inherited_style(base)?;
    let mut work = validate_scopes(source, scopes)?;
    let mut layers = configured_layers(scopes);
    let mut out = Vec::new();
    let mut stack: Vec<(String, CharStyle, Option<usize>)> = Vec::new();
    let mut style = base.clone();
    let bytes = source.as_bytes();
    let mut i = 0;
    let mut char_index = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'<' => {
                let end =
                    find_byte(bytes, i, b'>').ok_or_else(|| err(source, i, "unclosed '<' tag"))?;
                let inner = source.get(i + 1..end).unwrap_or("");
                if let Some(name) = inner.strip_prefix('/') {
                    let name = name.trim();
                    let Some((open_name, saved, layer)) = stack.pop() else {
                        return Err(err(source, i, &format!("</{name}> with nothing open")));
                    };
                    if !open_name.eq_ignore_ascii_case(name) {
                        return Err(err(source, i, &format!("</{name}> closes <{open_name}>")));
                    }
                    style = saved;
                    if let Some(index) = layer {
                        layers[index].range.1 = end + 1;
                        work = work.saturating_add(end + 1 - layers[index].range.0);
                        if work > MAX_SCOPED_STYLE_WORK {
                            return Err(style_error(
                                "scoped styles and markup exceed the 16777216-byte-work limit",
                            ));
                        }
                    }
                } else {
                    if stack.len() >= MAX_TAG_DEPTH {
                        return Err(err(
                            source,
                            i,
                            &format!("tag nesting exceeds the depth limit ({MAX_TAG_DEPTH})"),
                        ));
                    }
                    let (name, attrs) = split_tag(inner);
                    let mut new_style = style.clone();
                    let tag = parse_tag(source, i, &name, attrs)?;
                    // Scoped requests resolve the complete cascade below.
                    // Evaluating tags against the base alone could overflow
                    // a size that an enclosing configured scope replaces.
                    if scopes.is_empty() {
                        tag.apply_to(&mut new_style)?;
                    }
                    let layer = if scopes.is_empty() {
                        None
                    } else {
                        if layers.len() == MAX_SCOPED_STYLE_LAYERS {
                            return Err(style_error(
                                "scoped styles and markup exceed the 4096-layer limit",
                            ));
                        }
                        let index = layers.len();
                        layers.push(StyleLayer {
                            range: (i, i),
                            style: tag,
                        });
                        Some(index)
                    };
                    stack.push((name, std::mem::replace(&mut style, new_style), layer));
                }
                i = end + 1;
            }
            b'&' => {
                let end = find_byte(bytes, i, b';')
                    .filter(|e| e - i <= 12)
                    .ok_or_else(|| err(source, i, "unterminated entity (missing ';')"))?;
                let name = source.get(i + 1..end).unwrap_or("");
                let ch = decode_entity(name)
                    .ok_or_else(|| err(source, i, &format!("unknown entity &{name};")))?;
                out.push(StyledChar {
                    ch,
                    span: (i, end + 1),
                    char_index,
                    style: style.clone(),
                });
                char_index += 1;
                i = end + 1;
            }
            _ => {
                let ch = source[i..].chars().next().unwrap_or('\u{FFFD}');
                out.push(StyledChar {
                    ch,
                    span: (i, i + ch.len_utf8()),
                    char_index,
                    style: style.clone(),
                });
                char_index += 1;
                i += ch.len_utf8();
            }
        }
    }
    if let Some((open_name, _, _)) = stack.last() {
        return Err(err(
            source,
            source.len(),
            &format!("<{open_name}> is never closed"),
        ));
    }
    if !scopes.is_empty() {
        apply_scopes(&mut out, base, &layers)?;
    }
    Ok(out)
}

#[derive(Default)]
struct TagStyle<'a> {
    fields: StyleFields<'a>,
    monospace: bool,
    script: Option<Script>,
}

impl TagStyle<'_> {
    fn apply_to(&self, style: &mut CharStyle) -> Result<(), TextError> {
        self.fields.apply_validated(style)?;
        if self.monospace {
            style.mono = true;
            style.family = None;
        }
        if let Some(script) = self.script {
            style.script = script;
        }
        Ok(())
    }
}

struct StyleLayer<'a> {
    range: (usize, usize),
    style: TagStyle<'a>,
}

fn configured_layers<'a>(scopes: &[ScopedStyle<'a>]) -> Vec<StyleLayer<'a>> {
    scopes
        .iter()
        .map(|scope| StyleLayer {
            range: scope.range,
            style: TagStyle {
                fields: scope.fields.clone(),
                ..TagStyle::default()
            },
        })
        .collect()
}

fn contains(outer: (usize, usize), inner: (usize, usize)) -> bool {
    outer != inner && outer.0 <= inner.0 && inner.1 <= outer.1
}

fn apply_scopes(
    chars: &mut [StyledChar],
    base: &CharStyle,
    layers: &[StyleLayer<'_>],
) -> Result<(), TextError> {
    // Stable topological order gives containment priority without imposing an
    // invalid comparator on crossing ranges. Among currently unrelated scopes,
    // authored order wins; equal-range inline tags follow configured scopes.
    // The public layer bound keeps the pairwise dependency checks bounded and
    // avoids retaining a quadratic adjacency table.
    let mut parents: Vec<usize> = layers
        .iter()
        .map(|layer| {
            layers
                .iter()
                .filter(|parent| contains(parent.range, layer.range))
                .count()
        })
        .collect();
    let mut ready: BTreeSet<usize> = parents
        .iter()
        .enumerate()
        .filter_map(|(index, &count)| (count == 0).then_some(index))
        .collect();
    for character in chars.iter_mut() {
        character.style = base.clone();
    }
    while let Some(index) = ready.pop_first() {
        let layer = &layers[index];
        let first = chars.partition_point(|character| character.span.0 < layer.range.0);
        let end = chars.partition_point(|character| character.span.1 <= layer.range.1);
        for character in &mut chars[first..end.max(first)] {
            layer.style.apply_to(&mut character.style)?;
        }
        for (child, count) in parents.iter_mut().enumerate() {
            if contains(layer.range, layers[child].range) {
                *count -= 1;
                if *count == 0 {
                    ready.insert(child);
                }
            }
        }
    }
    Ok(())
}

fn parse_tag<'a>(
    source: &str,
    at: usize,
    name: &str,
    attrs: &'a str,
) -> Result<TagStyle<'a>, TextError> {
    let mut style = TagStyle::default();
    match name.to_ascii_lowercase().as_str() {
        "b" => style.fields.bold = Some(true),
        "i" => style.fields.italic = Some(true),
        "u" => style.fields.underline = Some(true),
        "s" => style.fields.strike = Some(true),
        "tt" => style.monospace = true,
        "big" => style.fields.size = Some(SizeChange::Relative(1.2)),
        "small" => style.fields.size = Some(SizeChange::Relative(5.0 / 6.0)),
        "sub" => {
            style.script = Some(Script::Sub);
            style.fields.size = Some(SizeChange::Relative(0.65));
        }
        "sup" => {
            style.script = Some(Script::Sup);
            style.fields.size = Some(SizeChange::Relative(0.65));
        }
        "span" => {}
        other => {
            return Err(err(source, at, &format!("unknown tag <{other}>")));
        }
    }
    if attrs.is_empty() {
        return Ok(style);
    }
    if !name.eq_ignore_ascii_case("span") {
        return Err(err(source, at, &format!("<{name}> takes no attributes")));
    }
    for (key, value) in parse_attrs(source, at, attrs)? {
        if value.len() > MAX_ATTR_LEN {
            return Err(err(source, at, &format!("attribute {key} is too long")));
        }
        match key.to_ascii_lowercase().as_str() {
            "foreground" | "fgcolor" | "color" => {
                let color = named_or_hex(value)
                    .ok_or_else(|| err(source, at, &format!("unrecognized color '{value}'")))?;
                style.fields.color = Some(color);
            }
            "weight" => match value.to_ascii_lowercase().as_str() {
                "bold" => style.fields.bold = Some(true),
                "normal" => style.fields.bold = Some(false),
                other => {
                    return Err(err(source, at, &format!("unknown weight '{other}'")));
                }
            },
            "style" => match value.to_ascii_lowercase().as_str() {
                "italic" | "oblique" => style.fields.italic = Some(true),
                "normal" => style.fields.italic = Some(false),
                other => {
                    return Err(err(source, at, &format!("unknown style '{other}'")));
                }
            },
            "underline" => match value.to_ascii_lowercase().as_str() {
                "none" => style.fields.underline = Some(false),
                "single" | "double" | "low" => style.fields.underline = Some(true),
                other => {
                    return Err(err(source, at, &format!("unknown underline '{other}'")));
                }
            },
            "font_family" | "face" | "font" => style.fields.family = Some(value),
            other => {
                return Err(err(
                    source,
                    at,
                    &format!("unknown <span> attribute '{other}'"),
                ));
            }
        }
    }
    Ok(style)
}

/// `key="value"` pairs (single or double quotes).
fn parse_attrs<'a>(
    source: &str,
    at: usize,
    attrs: &'a str,
) -> Result<Vec<(&'a str, &'a str)>, TextError> {
    let mut out = Vec::new();
    let mut rest = attrs.trim();
    while !rest.is_empty() {
        let eq = rest
            .find('=')
            .ok_or_else(|| err(source, at, &format!("expected key=\"value\" in '{rest}'")))?;
        let key = rest[..eq].trim();
        let after = rest[eq + 1..].trim_start();
        let quote = after
            .chars()
            .next()
            .filter(|c| *c == '"' || *c == '\'')
            .ok_or_else(|| err(source, at, &format!("attribute {key} needs a quoted value")))?;
        let body = &after[1..];
        let close = body.find(quote).ok_or_else(|| {
            err(
                source,
                at,
                &format!("attribute {key} has an unclosed quote"),
            )
        })?;
        out.push((key, &body[..close]));
        rest = body[close + 1..].trim_start();
    }
    Ok(out)
}

fn split_tag(inner: &str) -> (String, &str) {
    let inner = inner.trim();
    match inner.find(char::is_whitespace) {
        Some(pos) => (inner[..pos].to_owned(), inner[pos..].trim_start()),
        None => (inner.to_owned(), ""),
    }
}

fn decode_entity(name: &str) -> Option<char> {
    Some(match name {
        "amp" => '&',
        "lt" => '<',
        "gt" => '>',
        "quot" => '"',
        "apos" => '\'',
        _ => {
            let num = name.strip_prefix('#')?;
            let cp = if let Some(hex) = num.strip_prefix('x').or_else(|| num.strip_prefix('X')) {
                u32::from_str_radix(hex, 16).ok()?
            } else {
                num.parse::<u32>().ok()?
            };
            char::from_u32(cp)?
        }
    })
}

/// The color-name subset the markup accepts (manim's common names), plus
/// `#rgb`/`#rrggbb` hex.
fn named_or_hex(value: &str) -> Option<Srgb> {
    if value.starts_with('#') {
        return Srgb::from_hex(value).ok();
    }
    let hex = match value.to_ascii_lowercase().as_str() {
        "white" => "#FFFFFF",
        "black" => "#000000",
        "red" => "#FC6255",
        "green" => "#83C167",
        "blue" => "#58C4DD",
        "yellow" => "#FFFF00",
        "orange" => "#FF862F",
        "purple" => "#94424F",
        "pink" => "#D147BD",
        "teal" => "#5CD0B3",
        "maroon" => "#C55F73",
        "gold" => "#F0AC5F",
        "gray" | "grey" => "#888888",
        _ => return None,
    };
    Srgb::from_hex(hex).ok()
}

fn find_byte(bytes: &[u8], from: usize, needle: u8) -> Option<usize> {
    bytes[from..]
        .iter()
        .position(|b| *b == needle)
        .map(|p| from + p)
}

/// One-based line:column of a byte offset.
fn err(source: &str, at: usize, what: &str) -> TextError {
    let clamped = at.min(source.len());
    let before = &source[..clamped];
    let line = before.matches('\n').count() + 1;
    let col = before.chars().rev().take_while(|c| *c != '\n').count() + 1;
    TextError::Markup {
        what: what.to_owned(),
        line,
        col,
    }
}
