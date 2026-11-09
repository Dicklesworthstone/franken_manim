//! cargo run -p fmn --example live_bar_chart -- live-bars
//! Native PNG sequence: signed values, zero crossings, and live bar identities.

use fmn::prelude::*;

pub struct LiveBarChart;

impl SceneConstruct for LiveBarChart {
    fn construct(&mut self, stage: &mut Stage<'_>) -> fmn::Result<()> {
        let book = FontBook::bundled().map_err(fmn::library::TextMobjectError::Text)?;
        let mut chart = BarChart::new(vec![0.25, 0.0, -0.5])
            .height(2.0)
            .width(4.0)
            .bar_names(vec!["Alpha".into(), "Beta".into(), "Gamma".into()])
            .add_to(stage.arena_mut(), &book)?;
        stage.scene_mut().add(&[chart.mobject()])?;
        stage.wait(0.25)?;

        let config = AnimConfig {
            run_time: 0.5,
            rate_func: RateFunc::linear(),
            ..AnimConfig::default()
        };
        let first = ChangeBarValues::new(stage.arena(), &chart, &[0.75, -0.5, 0.0])?
            .with_config(config.clone());
        stage.play(first)?;
        stage.wait(0.25)?;

        let second =
            ChangeBarValues::new(stage.arena(), &chart, &[0.25, 0.9, -0.6])?.with_config(config);
        stage.play(second)?;
        // Prefix updates leave the remaining bars and all labels in place.
        chart.change_bar_values(stage.arena_mut(), &[0.0])?;
        stage.wait(0.25)?;
        Ok(())
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let output = std::env::args_os()
        .nth(1)
        .unwrap_or_else(|| "live-bars".into());
    let mut options = RenderOptions::new(output)?;
    options.format = RenderFormat::PngSequence;
    options.config.camera.resolution = (960, 540);
    options.config.camera.fps = 30;
    let report = render(&mut LiveBarChart, options)?;
    println!(
        "{} frames written to {}",
        report.artifact.frame_count,
        report.artifact.path.display(),
    );
    Ok(())
}
