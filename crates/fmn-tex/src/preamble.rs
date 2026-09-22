//! Additional preambles over fmd-math's existing bounded macro expander.
//! No second TeX parser: native source definitions remain authoritative.

use crate::{Mode, Span, TexEngine, TexError, Typeset};
use fmd_math::MathError;

/// Maximum additional-preamble UTF-8 bytes per typeset request.
pub const TEX_PREAMBLE_MAX_BYTES: usize = 65_536;
/// Maximum combined preamble, separator and formula UTF-8 bytes.
pub const TEX_PREAMBLE_SOURCE_MAX_BYTES: usize = 262_144;

impl TexEngine {
    /// Typeset with native inline declarations, exposing only formula spans.
    ///
    /// The preamble must independently parse without producing ink or spacing.
    /// A newline separates it from the formula so a trailing comment cannot
    /// swallow the expression. fmd-math owns definition/shadowing rules,
    /// expansion, recursion refusals and token budgets. Definitions are local
    /// to this request and never mutate the engine's pack.
    ///
    /// Cache entries retain the complete effective input and original native
    /// spans. Projection happens on an owned result after every cache lookup,
    /// so preamble changes cannot reuse another macro expansion or span map.
    /// Empty preambles take the unchanged ordinary typeset path.
    ///
    /// # Errors
    /// [`TexError::Preamble`] for invalid/visible/over-budget preambles;
    /// otherwise the ordinary native error, rebased to the formula's bytes.
    pub fn typeset_with_preamble(
        &self,
        mode: Mode,
        source: &str,
        preamble: &str,
    ) -> Result<Typeset, TexError> {
        if preamble.is_empty() {
            return self.typeset(mode, source);
        }
        let invalid = |what: &str| TexError::Preamble {
            what: what.to_owned(),
        };
        if preamble.len() > TEX_PREAMBLE_MAX_BYTES {
            return Err(invalid("source exceeds 65536 UTF-8 bytes"));
        }
        let offset = preamble.len() + 1;
        let length = offset
            .checked_add(source.len())
            .filter(|length| *length <= TEX_PREAMBLE_SOURCE_MAX_BYTES)
            .ok_or_else(|| invalid("combined source exceeds 262144 UTF-8 bytes"))?;
        let declarations = self
            .typeset(mode, preamble)
            .map_err(|error| TexError::Preamble {
                what: error.to_string(),
            })?;
        let layout = declarations.layout;
        if !declarations.subs.is_empty()
            || layout.width != 0.0
            || layout.height != 0.0
            || layout.depth != 0.0
        {
            return Err(invalid(
                "must contain declarations, not visible content or spacing",
            ));
        }
        let mut effective = String::new();
        effective
            .try_reserve_exact(length)
            .map_err(|_| invalid("could not reserve combined source storage"))?;
        effective.push_str(preamble);
        effective.push('\n');
        effective.push_str(source);
        let mut layout = self
            .typeset(mode, &effective)
            .map_err(|error| rebase_error(error, offset))?
            .layout;
        let project = |span: &mut Span| -> Result<(), TexError> {
            let start = span.start.checked_sub(offset);
            let end = span.end.checked_sub(offset);
            match (start, end) {
                (Some(start), Some(end))
                    if start <= end
                        && source.is_char_boundary(start)
                        && source.is_char_boundary(end) =>
                {
                    *span = Span::new(start, end);
                    Ok(())
                }
                _ => Err(invalid(
                    "native expansion produced a span outside the formula; \
                     see UPSTREAM_LEDGER.md #13 for definition-nested macros",
                )),
            }
        };
        for glyph in &mut layout.glyphs {
            project(&mut glyph.span)?;
        }
        for rule in &mut layout.rules {
            project(&mut rule.span)?;
        }
        for path in &mut layout.paths {
            project(&mut path.span)?;
        }
        Typeset::from_borrowed(source, layout).map_err(TexError::from)
    }
}

fn rebase_error(error: TexError, offset: usize) -> TexError {
    let TexError::Math(mut error) = error else {
        return error;
    };
    if error.span().start < offset {
        return TexError::Preamble {
            what: error.to_string(),
        };
    }
    match &mut error {
        MathError::Malformed { at, .. } => *at -= offset,
        MathError::UnsupportedCommand { span, .. } | MathError::UnmappedChar { span, .. } => {
            span.start -= offset;
            span.end = span.end.saturating_sub(offset);
        }
    }
    TexError::Math(error)
}
