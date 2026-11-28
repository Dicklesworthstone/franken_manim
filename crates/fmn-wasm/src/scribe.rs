//! Browser-local typesetting with the same bundled faces, mobject builders,
//! and Lumen renderer as native scenes. Opt in with the `scribe` feature.
use fmn_anim::timeline::Timeline;
use fmn_core::color::Srgb;
use fmn_core::rng::RngRoot;
use fmn_library::{FontBook, SpanMapData, Tex, TexEngine, TexText, Text, VStyle};
use fmn_mobject::{Mob, Mobject, Stage};
use wasm_bindgen::prelude::{JsError, wasm_bindgen};

use crate::{SurfaceError, render_stage_rgba8, render_stage_rgba8_into, validate_dimensions};

/// Reusable font resources for browser-local text, markup, and mathematics.
#[wasm_bindgen]
pub struct FmnScribe {
    book: FontBook,
    engine: TexEngine,
}

#[wasm_bindgen]
impl FmnScribe {
    /// Load the bundled faces once. The optional module needs no system fonts.
    #[wasm_bindgen(constructor)]
    pub fn new() -> Result<Self, JsError> {
        Ok(Self {
            book: FontBook::bundled()?,
            engine: TexEngine::new("fmd-math/pack/default", None)?,
        })
    }

    /// Typeset a centered object. Kinds are `text`, `markup`, `math` and `textext` (prose with inline math).
    pub fn typeset(&self, kind: &str, source: &str, font_size: f64) -> Result<FmnTypeset, JsError> {
        Ok(self.layout(kind, source, font_size)?)
    }
}

impl FmnScribe {
    fn build_with_spans(
        &self,
        kind: &str,
        source: &str,
        font_size: f64,
    ) -> Result<(Mobject, SpanMapData), SurfaceError> {
        if !font_size.is_finite() || font_size <= 0.0 {
            return Err(SurfaceError("font size must be finite and positive".into()));
        }
        match kind {
            "text" | "markup" => {
                let text = if kind == "markup" {
                    Text::markup(source)
                } else {
                    Text::new(source)
                };
                let built = text
                    .font_size(font_size)
                    .build(&self.book)
                    .map_err(|e| SurfaceError::new("text layout", e))?;
                let spans = built.span_map();
                Ok((built.into(), spans))
            }
            "math" | "textext" => {
                let built = if kind == "math" {
                    Tex::new(source)
                        .display()
                        .font_size(font_size)
                        .build(&self.engine)
                } else {
                    TexText::new(source)
                        .font_size(font_size)
                        .build(&self.engine)
                }
                .map_err(|e| SurfaceError::new("TeX layout", e))?;
                let spans = built.span_map();
                Ok((built.into(), spans))
            }
            _ => Err(SurfaceError(format!(
                "unknown typesetting kind {kind:?}; expected text, markup, math, or textext"
            ))),
        }
    }

    fn layout(&self, kind: &str, source: &str, font_size: f64) -> Result<FmnTypeset, SurfaceError> {
        let (object, spans) = self.build_with_spans(kind, source, font_size)?;
        let mut stage = Stage::new();
        let root = stage.add(object);
        stage
            .add_to_scene(root)
            .map_err(|e| SurfaceError::new("add typeset object", e))?;
        Ok(FmnTypeset { stage, root, spans })
    }
}

/// A typeset object that can be redrawn at different resolutions without relayout.
#[wasm_bindgen]
pub struct FmnTypeset {
    stage: Stage,
    root: Mob,
    spans: SpanMapData,
}

impl FmnTypeset {
    // DOM text selections use UTF-16 code units. Native provenance uses UTF-8 bytes.
    fn selected_parts(&self, start: u32, end: u32) -> Result<Vec<usize>, SurfaceError> {
        if start > end {
            return Err(SurfaceError("selection start exceeds its end".into()));
        }
        let byte_offset = |target: u32| {
            let mut units = 0_usize;
            for (byte, ch) in self.spans.source.char_indices() {
                if units == target as usize {
                    return Ok(byte);
                }
                units += ch.len_utf16();
                if units > target as usize {
                    return Err(SurfaceError(
                        "selection splits a UTF-16 surrogate pair".into(),
                    ));
                }
            }
            if units == target as usize {
                Ok(self.spans.source.len())
            } else {
                Err(SurfaceError("selection extends beyond the source".into()))
            }
        };
        let range = (byte_offset(start)?, byte_offset(end)?);
        if start == end {
            return Ok(Vec::new());
        }
        Ok(self.spans.select(range))
    }
}

#[wasm_bindgen]
impl FmnTypeset {
    /// Native primitive ordinals wholly inside a half-open source selection.
    /// Offsets are UTF-16 code units as in a textarea's selectionStart/End.
    pub fn select_range(&self, start: u32, end: u32) -> Result<Vec<u32>, JsError> {
        self.selected_parts(start, end)?
            .into_iter()
            .map(|index| u32::try_from(index).map_err(JsError::from))
            .collect()
    }

    /// Color only the native glyphs, rules and paths wholly owned by the selection.
    /// Keeps geometry, opacity, other source occurrences and markup decorations intact.
    /// Returns the number of primitives changed. Exports include the new paint.
    pub fn color_range(&mut self, start: u32, end: u32, color: &str) -> Result<usize, JsError> {
        let color = Srgb::from_hex(color)?;
        let parts = self.selected_parts(start, end)?;
        for &index in &parts {
            // Neither geometry nor child order can change after construction.
            let child = self
                .stage
                .get(self.root)
                .expect("typeset root")
                .submobjects()[index];
            self.stage.set_color(child, color, None, false);
        }
        Ok(parts.len())
    }

    /// Draw the object over opaque black using the native eight-unit viewport.
    pub fn render(&self, width: u32, height: u32) -> Result<Vec<u8>, JsError> {
        validate_dimensions(width, height)?;
        Ok(render_stage_rgba8(&self.stage, width, height, 0)?)
    }

    /// Export the typeset outlines as a single-frame FMTL timeline. The
    /// standard player can open it without loading fonts or Scribe.
    pub fn to_bundle(&self) -> Result<Vec<u8>, JsError> {
        let mut timeline = Timeline::new(1)?;
        timeline.wait(1.0)?;
        let mut stage = self.stage.snapshot().materialize();
        Ok(fmn_scene::export_timeline_bundle(
            timeline,
            &mut stage,
            &RngRoot::from_seed(0),
        )?)
    }

    /// Draw into an existing, tightly packed RGBA8 buffer.
    pub fn render_into(&self, width: u32, height: u32, dst: &mut [u8]) -> Result<(), JsError> {
        validate_dimensions(width, height)?;
        Ok(render_stage_rgba8_into(&self.stage, width, height, 0, dst)?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn authored_inputs_produce_distinct_visible_repeatable_ink() {
        let scribe = FmnScribe::new().unwrap();
        let mut frames = Vec::new();
        for (kind, source) in [
            ("text", "Velocity"),
            ("markup", "<b>bold</b>"),
            ("math", r"\frac{x^2}{2}"),
        ] {
            let object = scribe.layout(kind, source, 72.0).unwrap();
            let frame = object.render(240, 128).unwrap();
            assert!(frame.chunks_exact(4).any(|p| p[0] > 0));
            let mut dst = vec![0; frame.len()];
            object.render_into(240, 128, &mut dst).unwrap();
            assert_eq!(frame, dst);
            let bytes = object.to_bundle().unwrap();
            let mut player = crate::player::FmnPlayer::from_bundle(&bytes).unwrap();
            assert_eq!(player.frame_count(), 1);
            player.set_viewport(240, 128).unwrap();
            assert_eq!(frame, player.render_frame(0).unwrap());
            assert!(!frames.contains(&frame));
            frames.push(frame);
        }
        assert!(scribe.layout("math", r"\DoesNotExist", 48.0).is_err());
        assert!(scribe.layout("text", "", f64::NAN).is_err());
    }

    #[test]
    fn a_fraction_can_color_just_one_of_two_identical_terms_and_replay_it() {
        let scribe = FmnScribe::new().unwrap();
        let mut item = scribe.layout("math", r"\frac{x}{x}", 100.0).unwrap();
        let before = item.render(320, 160).unwrap();
        let numerator = item.select_range(6, 7).unwrap();
        let denominator = item.select_range(9, 10).unwrap();
        assert_eq!(numerator.len(), 1);
        assert_eq!(denominator.len(), 1);
        assert_ne!(numerator, denominator);
        assert!(item.select_range(0, 11).unwrap().len() > 2);
        assert_eq!(item.color_range(6, 7, "#F00").unwrap(), 1);
        let after = item.render(320, 160).unwrap();
        let row_bytes = 320 * 4;
        let mut changed = 0;
        for (index, (p, q)) in before
            .chunks_exact(4)
            .zip(after.chunks_exact(4))
            .enumerate()
        {
            if p != q {
                assert!(index * 4 / row_bytes < 80, "only the numerator changes");
                assert!(q[0] > q[1] && q[0] > q[2]);
                changed += 1;
            }
        }
        assert!(changed > 0);
        let mut player = crate::player::FmnPlayer::from_bundle(&item.to_bundle().unwrap()).unwrap();
        player.set_viewport(320, 160).unwrap();
        assert_eq!(player.render_frame(0).unwrap(), after);
    }

    #[test]
    fn markup_selection_uses_dom_offsets_and_whole_entities() {
        let scribe = FmnScribe::new().unwrap();
        let source = "<b>é&amp;𝒜</b>!";
        let item = scribe.layout("markup", source, 72.0).unwrap();
        assert_eq!(item.select_range(3, 4).unwrap(), vec![0]);
        assert_eq!(item.select_range(4, 9).unwrap(), vec![1]);
        assert!(item.select_range(5, 8).unwrap().is_empty());
        assert_eq!(item.select_range(9, 11).unwrap(), vec![2]);
        assert!(item.selected_parts(9, 10).is_err()); // Inside the surrogate pair.
        assert!(item.selected_parts(5, 4).is_err());
        assert!(item.selected_parts(17, 17).is_err());
        assert!(item.select_range(0, 3).unwrap().is_empty()); // Markup owns no glyph.
        assert_eq!(item.select_range(15, 16).unwrap(), vec![3]);
        assert!(item.select_range(16, 16).unwrap().is_empty());
    }

    #[test]
    fn prose_with_inline_math_preserves_island_source_spans() {
        let scribe = FmnScribe::new().unwrap();
        let mut item = scribe.layout("textext", r"Rate $\pi$ Hz", 72.0).unwrap();
        assert_eq!(item.spans.source.as_ref(), r"Rate $\pi$ Hz");
        assert!(item.select_range(7, 9).unwrap().is_empty());
        assert_eq!(item.color_range(6, 9, "#0088ff").unwrap(), 1);
        assert!(
            item.render(240, 128)
                .unwrap()
                .chunks_exact(4)
                .any(|p| p[2] > p[0])
        );
    }
}
