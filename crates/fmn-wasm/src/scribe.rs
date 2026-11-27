//! Browser-local typesetting with the same bundled faces, mobject builders,
//! and Lumen renderer as native scenes. Opt in with the `scribe` feature.
use fmn_anim::timeline::Timeline;
use fmn_core::rng::RngRoot;
use fmn_library::{FontBook, Tex, TexEngine, Text};
use fmn_mobject::{Mobject, Stage};
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

    /// Typeset a centered object. Kinds are `text`, `markup`, and `math`.
    pub fn typeset(&self, kind: &str, source: &str, font_size: f64) -> Result<FmnTypeset, JsError> {
        Ok(self.layout(kind, source, font_size)?)
    }
}

impl FmnScribe {
    fn layout(&self, kind: &str, source: &str, font_size: f64) -> Result<FmnTypeset, SurfaceError> {
        if !font_size.is_finite() || font_size <= 0.0 {
            return Err(SurfaceError("font size must be finite and positive".into()));
        }
        let object: Mobject = match kind {
            "text" | "markup" => {
                let text = if kind == "markup" {
                    Text::markup(source)
                } else {
                    Text::new(source)
                };
                text.font_size(font_size)
                    .build(&self.book)
                    .map_err(|e| SurfaceError::new("text layout", e))?
                    .into()
            }
            "math" => Tex::new(source)
                .display()
                .font_size(font_size)
                .build(&self.engine)
                .map_err(|e| SurfaceError::new("math layout", e))?
                .into(),
            _ => {
                return Err(SurfaceError(format!(
                    "unknown typesetting kind {kind:?}; expected text, markup, or math"
                )));
            }
        };
        let mut stage = Stage::new();
        let root = stage.add(object);
        stage
            .add_to_scene(root)
            .map_err(|e| SurfaceError::new("add typeset object", e))?;
        Ok(FmnTypeset { stage })
    }
}

/// A typeset object that can be redrawn at different resolutions without relayout.
#[wasm_bindgen]
pub struct FmnTypeset {
    stage: Stage,
}

#[wasm_bindgen]
impl FmnTypeset {
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
}
