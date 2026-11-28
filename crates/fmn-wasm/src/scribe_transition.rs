//! Author a portable Choreo transition from two already typeset, possibly styled objects.
use super::FmnTypeset;
use crate::SurfaceError;
use fmn_anim::{Animation, timeline::Timeline};
use fmn_core::rng::RngRoot;
use fmn_library::{TextMatchingConfig, TransformMatchingStrings, TransformMatchingTex};
use fmn_mobject::Stage;
use fmn_scene::{BundleExportLimits, export_timeline_bundle_with_limits};
use wasm_bindgen::prelude::{JsError, wasm_bindgen};

// Interactive export captures geometry for every frame of a matching transform.
const MAX_FRAMES: u64 = 602;
const MAX_CAPTURE_BYTES: usize = 64 * 1024 * 1024;

#[wasm_bindgen]
impl FmnTypeset {
    /// Animate this object into another and export a playable FMTL bundle.
    /// Matched TeX parts or text runs move together, and unmatched parts fade.
    /// The initial and final frames hold the exact operands, including styles.
    /// Neither operand is mutated. The browser exporter accepts up to 600
    /// motion frames plus two endpoint frames and 64 MiB of capture data.
    pub fn transition_to(
        &self,
        target: &FmnTypeset,
        fps: u32,
        seconds: f64,
    ) -> Result<Vec<u8>, JsError> {
        self.transition_bundle(target, fps, seconds)
            .map_err(|e| JsError::new(&e.to_string()))
    }
}

impl FmnTypeset {
    fn transition_bundle(
        &self,
        target: &Self,
        fps: u32,
        seconds: f64,
    ) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
        if !seconds.is_finite() || seconds <= 0.0 {
            return Err(
                SurfaceError("transition duration must be finite and positive".into()).into(),
            );
        }
        let mut timeline = Timeline::new(fps)?;
        let mut stage = Stage::new();
        let source_root = self.stage.copy_into(self.root, &mut stage)?;
        let target_root = target.stage.copy_into(target.root, &mut stage)?;
        stage.add_to_scene(source_root)?;
        let config = TextMatchingConfig {
            run_time: seconds,
            ..TextMatchingConfig::default()
        };
        let animation: Box<dyn Animation> = if self.matches_tex && target.matches_tex {
            Box::new(TransformMatchingTex::with_config(
                &mut stage,
                source_root,
                target_root,
                &self.spans,
                &target.spans,
                config,
            )?)
        } else {
            Box::new(TransformMatchingStrings::with_config(
                &mut stage,
                source_root,
                target_root,
                &self.spans,
                &target.spans,
                config,
            )?)
        };

        timeline.label("Start").wait_frames(1)?;
        timeline.label("Transform").play(vec![animation])?;
        timeline.label("Result").wait_frames(1)?;
        Ok(export_timeline_bundle_with_limits(
            timeline,
            &mut stage,
            &RngRoot::from_seed(0),
            BundleExportLimits {
                max_frames: MAX_FRAMES,
                max_capture_bytes: MAX_CAPTURE_BYTES,
            },
        )?)
    }
}

#[cfg(test)]
mod tests {
    use crate::{player::FmnPlayer, scribe::FmnScribe};

    #[test]
    fn matching_transitions_replay_exact_endpoints_and_distinct_intermediate_ink() {
        let book = FmnScribe::new().unwrap();
        for (kind, before, after) in [("math", "x+y", "y+x^2"), ("text", "AB CD", "CD AB!")] {
            let mut from = book.typeset(kind, before, 72.0).unwrap();
            let mut to = book.typeset(kind, after, 72.0).unwrap();
            assert_eq!(from.color_range(0, 1, "#2277ff").unwrap(), 1);
            assert_eq!(to.color_range(0, 1, "#ff7722").unwrap(), 1);
            let start = from.render(240, 128).unwrap();
            let result = to.render(240, 128).unwrap();
            let bytes = from.transition_bundle(&to, 12, 0.5).unwrap();
            let mut movie = FmnPlayer::from_bundle(&bytes).unwrap();
            movie.set_viewport(240, 128).unwrap();
            assert_eq!(movie.frame_count(), 8);
            assert_eq!(movie.frame_of_label("Start"), Some(0));
            assert_eq!(movie.frame_of_label("Result"), Some(7));
            assert!(movie.render_frame(7).unwrap() == result);
            let middle = movie.render_frame(3).unwrap();
            assert!(middle != start && middle != result);
            assert!(movie.render_frame(0).unwrap() == start);
            assert!(from.render(240, 128).unwrap() == start);
            assert!(to.render(240, 128).unwrap() == result);
            assert!(from.transition_bundle(&to, 0, 1.0).is_err());
            assert!(from.transition_bundle(&to, 60, 11.0).is_err());
            assert!(from.transition_bundle(&to, 30, f64::NAN).is_err());
        }
    }
}
