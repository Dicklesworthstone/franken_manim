//! cargo run -p fmn --example matching_text -- media/matching_text
//! Native equation and prose matching, ready for durable declarations.
//!
//! `into_native` consumes a fresh matching plan before it is scheduled. It
//! preserves the span matches and original-target replacement while making the
//! animation available to the native checkpoint/resume machinery.

use fmn::prelude::*;

pub struct DurableMatchingText;

impl SceneConstruct for DurableMatchingText {
    fn construct(&mut self, stage: &mut Stage<'_>) -> fmn::Result<()> {
        let source = Tex::new(r"a^2+b^2=c^2")
            .font_size(36.0)
            .build(stage.tex_engine()?)?;
        let target = Tex::new(r"c^2=a^2+b^2")
            .font_size(36.0)
            .build(stage.tex_engine()?)?;
        let (source_spans, target_spans) = (source.span_map(), target.span_map());
        let source = stage.add(source)?;
        // Replacement targets stay detached until successful animation cleanup.
        let target = stage.arena_mut().add(target);
        stage.move_to(source, [-0.4, 0.8, 0.0], ORIGIN);
        stage.move_to(target, [0.3, 0.8, 0.0], ORIGIN);
        let config = TextMatchingConfig {
            run_time: 0.5,
            lag_ratio: 0.2,
            ..TextMatchingConfig::default()
        };
        let matching = TransformMatchingTex::with_config(
            stage.arena_mut(),
            source,
            target,
            &source_spans,
            &target_spans,
            config.clone(),
        )?
        .into_native(stage.arena())?;
        stage.play(matching)?;

        let book = FontBook::bundled().map_err(fmn::library::TextMobjectError::Text)?;
        let before = Text::new("AB CD")
            .font("IBM Plex Sans")
            .font_size(32.0)
            .build(&book)?;
        let after = Text::new("CD AB!")
            .font("IBM Plex Sans")
            .font_size(32.0)
            .build(&book)?;
        let (before_spans, after_spans) = (before.span_map(), after.span_map());
        let before = stage.add(before)?;
        let after = stage.arena_mut().add(after);
        stage.move_to(before, [-0.4, -0.8, 0.0], ORIGIN);
        stage.move_to(after, [0.3, -0.8, 0.0], ORIGIN);
        let matching = TransformMatchingStrings::with_config(
            stage.arena_mut(),
            before,
            after,
            &before_spans,
            &after_spans,
            config,
        )?
        .into_native(stage.arena())?;
        stage.play(matching)?;
        Ok(())
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let output = std::env::args_os()
        .nth(1)
        .unwrap_or_else(|| "media/matching_text".into());
    let mut options = RenderOptions::new(output)?;
    options.format = RenderFormat::PngSequence;
    options.config.camera.resolution = (960, 540);
    options.config.camera.fps = 30;
    let report = render(&mut DurableMatchingText, options)?;
    println!(
        "{} frames written to {}",
        report.artifact.frame_count,
        report.artifact.path.display(),
    );
    Ok(())
}
