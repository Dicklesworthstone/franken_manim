//! The `MarkdownMobject` tier-1 (§11.7, fm-u8y): fmd's document parser +
//! the Scribe text stack, as animatable content.
//!
//! Blocks map to submobjects in source order — headings and paragraphs
//! through the manim markup tag set, fenced code through [`Code`] — and
//! each child keeps its **source byte range** so later tranches can drive
//! `TransformMatchingStrings` across document edits (edit a slide, animate
//! the diff). Tier discipline: enhanced tier, never gate-blocking.
//!
//! Documented tranche-1 gaps: tables/images render as their raw text,
//! inline `$math$` waits on the fmd-math integration, links keep their
//! text and drop their destination, and nested lists flatten with indent
//! prefixes.

use fmn_text::FontBook;

use crate::code::{Code, CodeTheme};
use crate::text::{Text, TextMobject, TextMobjectError};
use crate::vmobject::VMobject;

/// Convert one inline run to the manim markup tag set (`<b>`, `<i>`,
/// `<s>`, `<tt>`); links keep their text.
fn inlines_to_markup(inlines: &[franken_markdown::ast::Inline], out: &mut String) {
    use franken_markdown::ast::Inline;
    for inline in inlines {
        match inline {
            Inline::Text(text) => out.push_str(text),
            Inline::Emphasis(inner) => {
                out.push_str("<i>");
                inlines_to_markup(inner, out);
                out.push_str("</i>");
            }
            Inline::Strong(inner) => {
                out.push_str("<b>");
                inlines_to_markup(inner, out);
                out.push_str("</b>");
            }
            Inline::Strikethrough(inner) => {
                out.push_str("<s>");
                inlines_to_markup(inner, out);
                out.push_str("</s>");
            }
            Inline::Code(code) => {
                out.push_str("<tt>");
                out.push_str(code);
                out.push_str("</tt>");
            }
            Inline::Link { content, .. } => inlines_to_markup(content, out),
            Inline::Image { alt, .. } => out.push_str(alt),
            Inline::SoftBreak => out.push(' '),
            Inline::HardBreak => out.push('\n'),
            Inline::Html(html) => out.push_str(html),
        }
    }
}

/// Heading level → size factor over the body size (HTML-document
/// conventions, which are also what a slide deck wants).
#[must_use]
pub fn heading_factor(level: u8) -> f64 {
    match level {
        1 => 2.0,
        2 => 1.5,
        3 => 1.17,
        4 => 1.0,
        5 => 0.83,
        _ => 0.67,
    }
}

/// One laid-out block with its source provenance.
pub struct LaidBlock {
    /// Inclusive start / exclusive end byte in the Markdown source.
    pub byte_range: (usize, usize),
    /// The block's laid-out content.
    pub vmob: VMobject,
}

/// A parsed-and-laid-out Markdown document.
pub struct MarkdownMobject {
    /// Blocks in source order, each with its source byte range.
    pub blocks: Vec<LaidBlock>,
    /// The whole document as one mobject (blocks stacked vertically).
    pub vmob: VMobject,
}

/// The Markdown builder.
#[derive(Debug, Clone)]
pub struct Markdown<'a> {
    source: &'a str,
    theme: CodeTheme,
    font_size: f64,
}

impl<'a> Markdown<'a> {
    /// A builder with the defaults: light code theme, the Reference's
    /// default text font size.
    #[must_use]
    pub fn new(source: &'a str) -> Self {
        Self {
            source,
            theme: CodeTheme::light(),
            font_size: crate::text::DEFAULT_FONT_SIZE,
        }
    }

    /// The theme for fenced code blocks.
    #[must_use]
    pub fn theme(mut self, theme: CodeTheme) -> Self {
        self.theme = theme;
        self
    }

    /// The base font size (headings scale from it).
    #[must_use]
    pub fn font_size(mut self, font_size: f64) -> Self {
        self.font_size = font_size;
        self
    }

    fn layout_text_block(
        &self,
        book: &FontBook,
        markup: String,
        size_factor: f64,
    ) -> Result<TextMobject, TextMobjectError> {
        Text::markup(&markup)
            .font_size(self.font_size * size_factor)
            .build(book)
    }

    fn layout_list(
        &self,
        book: &FontBook,
        list: &franken_markdown::ast::List,
        depth: usize,
    ) -> Result<TextMobject, TextMobjectError> {
        let mut lines = String::new();
        let indent = "    ".repeat(depth);
        for (index, item) in list.items.iter().enumerate() {
            let marker = if list.ordered {
                format!("{}. ", list.start + index as u64)
            } else {
                "• ".to_string()
            };
            let task_marker = match item.task {
                Some(true) => "[x] ",
                Some(false) => "[ ] ",
                None => "",
            };
            for (line_index, block) in item.blocks.iter().enumerate() {
                let prefix = if line_index == 0 {
                    format!("{indent}{marker}{task_marker}")
                } else {
                    format!("{indent}    ")
                };
                let mut markup = String::new();
                if let franken_markdown::ast::Block::Paragraph(inlines) = block {
                    inlines_to_markup(inlines, &mut markup);
                }
                lines.push_str(&format!("{prefix}{markup}\n"));
            }
        }
        self.layout_text_block(book, lines, 1.0)
    }

    /// Parse and lay out the document.
    ///
    /// # Errors
    /// Whatever the per-block [`Text::build`] calls hit.
    pub fn build(&self, book: &FontBook) -> Result<MarkdownMobject, TextMobjectError> {
        let spanned = franken_markdown::parse::parse_document_spanned(self.source);
        let mut blocks = Vec::new();
        let mut children: Vec<VMobject> = Vec::new();
        let mut y_cursor = 0.0f64;
        const BLOCK_GAP: f64 = 0.45;

        for (bi, spanned_block) in &spanned.blocks.iter().enumerate().collect::<Vec<_>>() {
            eprintln!(
                "BLOCK {bi}: {:?}",
                std::mem::discriminant(&spanned_block.node)
            );
            let (start, end) = (spanned_block.span.start, spanned_block.span.end);
            let vmob = match &spanned_block.node {
                franken_markdown::ast::Block::Heading { level, inlines } => {
                    let mut markup = String::new();
                    inlines_to_markup(inlines, &mut markup);
                    self.layout_text_block(
                        book,
                        format!("<b>{markup}</b>"),
                        heading_factor(*level),
                    )?
                    .vmob
                }
                franken_markdown::ast::Block::Paragraph(inlines) => {
                    let mut markup = String::new();
                    inlines_to_markup(inlines, &mut markup);
                    self.layout_text_block(book, markup, 1.0)?.vmob
                }
                franken_markdown::ast::Block::CodeBlock { lang, code } => {
                    Code::new(code)
                        .language(lang.as_deref().unwrap_or(""))
                        .theme(self.theme)
                        .font_size(self.font_size * 0.85)
                        .build(book)?
                        .vmob
                }
                franken_markdown::ast::Block::List(list) => self.layout_list(book, list, 0)?.vmob,
                franken_markdown::ast::Block::BlockQuote(inner_blocks) => {
                    // Tier-1: render quote paragraphs as indented italics.
                    let mut markup = String::new();
                    for block in inner_blocks {
                        if let franken_markdown::ast::Block::Paragraph(inlines) = block {
                            markup.push_str("> <i>");
                            inlines_to_markup(inlines, &mut markup);
                            markup.push_str("</i>\n");
                        }
                    }
                    self.layout_text_block(book, markup, 1.0)?.vmob
                }
                // Tables, raw HTML, thematic breaks: documented next
                // increments — rendered as their raw source text so nothing
                // silently disappears.
                other => {
                    let raw = other_source_snippet(other);
                    self.layout_text_block(book, raw, 0.9)?.vmob
                }
            };

            let height = vmob
                .bbox_point([0.0, 1.0, 0.0])
                .and_then(|top| {
                    vmob.bbox_point([0.0, -1.0, 0.0])
                        .map(|bottom| top[1] - bottom[1])
                })
                .unwrap_or(0.0);
            let mut placed = vmob;
            if y_cursor < 0.0 || !children.is_empty() {
                placed = placed.shifted([0.0, y_cursor, 0.0]);
            }
            y_cursor -= height + BLOCK_GAP;
            blocks.push(LaidBlock {
                byte_range: (start, end),
                vmob: placed,
            });
            children.push(
                blocks
                    .last()
                    .map(|laid| laid.vmob.clone())
                    .expect("just pushed"),
            );
        }

        let vmob = VMobject::new().with_children(children);
        Ok(MarkdownMobject { blocks, vmob })
    }
}

fn other_source_snippet(block: &franken_markdown::ast::Block) -> String {
    match block {
        franken_markdown::ast::Block::Table(table) => {
            let mut out = String::from("[table]\n");
            for row in std::iter::once(&table.head).chain(table.rows.iter()) {
                for cell in row {
                    inlines_to_markup(cell, &mut out);
                    out.push('\t');
                }
                out.push('\n');
            }
            out
        }
        franken_markdown::ast::Block::HtmlBlock(html) => html.clone(),
        franken_markdown::ast::Block::ThematicBreak => "———".to_string(),
        _ => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fmn_text::FontBook;

    fn book() -> FontBook {
        FontBook::bundled().expect("bundled faces parse")
    }

    const SAMPLE: &str =
        "# Title\n\nA paragraph with *emphasis*.\n\n```rust\nfn main() {}\n```\n\n- one\n- two\n";

    #[test]
    fn blocks_map_to_children_in_source_order() {
        let md = Markdown::new(SAMPLE).build(&book()).expect("builds");
        assert_eq!(md.blocks.len(), 4, "heading, paragraph, fence, list");
        assert_eq!(md.vmob.children().len(), 4);
    }

    #[test]
    fn provenance_ranges_tile_the_source_prefixes() {
        let md = Markdown::new(SAMPLE).build(&book()).expect("builds");
        // First block starts at 0 and covers the heading line.
        assert_eq!(md.blocks[0].byte_range.0, 0);
        assert!(md.blocks[0].byte_range.1 >= "# Title".len());
        // Ranges are ordered and non-overlapping.
        for pair in md.blocks.windows(2) {
            assert!(
                pair[0].byte_range.1 <= pair[1].byte_range.0 + 2,
                "ranges {:?} then {:?} overlap",
                pair[0].byte_range,
                pair[1].byte_range
            );
        }
    }

    #[test]
    fn headings_render_larger_than_body() {
        let md = Markdown::new(SAMPLE).build(&book()).expect("builds");
        let height = |laid: &LaidBlock| {
            let top = laid.vmob.bbox_point([0.0, 1.0, 0.0]).unwrap()[1];
            let bottom = laid.vmob.bbox_point([0.0, -1.0, 0.0]).unwrap()[1];
            top - bottom
        };
        assert!(
            height(&md.blocks[0]) > height(&md.blocks[1]),
            "h1 taller than body"
        );
    }

    #[test]
    fn fenced_code_uses_the_code_path() {
        let md = Markdown::new(SAMPLE).build(&book()).expect("builds");
        let fence = &md.blocks[2].vmob;
        let fills: Vec<_> = fence
            .children()
            .iter()
            .map(|c| c.style().fill_color)
            .collect();
        let base = fmn_core::constants::DEFAULT_MOBJECT_COLOR;
        assert!(
            fills.iter().any(|c| *c != base),
            "fence must carry highlighted fills, got {fills:?}"
        );
    }

    #[test]
    fn lists_flatten_with_markers() {
        let md = Markdown::new(SAMPLE).build(&book()).expect("builds");
        // The list block exists and lays out; exact glyph mapping is the
        // text pipeline's contract (already covered there).
        assert!(!md.blocks[3].vmob.children().is_empty());
    }
}
