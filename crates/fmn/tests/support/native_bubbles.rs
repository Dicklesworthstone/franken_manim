//! Native bubble composition through the public scene-to-PNG boundary.
use fmn::library::drawings::speech_bubble_default;
use fmn::prelude::*;
use fmn_platform::fs::{FileSystem, VirtualFs};
use std::{path::Path, sync::Arc};

struct SpeechScene {
    detached: bool,
}

impl SceneConstruct for SpeechScene {
    fn construct(&mut self, stage: &mut Stage<'_>) -> fmn::Result<()> {
        // Asymmetric content makes an accidental content flip visible.
        let content = Polygon::new([[-0.5, -0.25, 0.0], [0.5, -0.25, 0.0], [-0.5, 0.25, 0.0]])
            .style(Style::default().fill(BLUE, 1.0).stroke(BLUE, 0.0, 1.0))
            .build();
        let root = if self.detached {
            // The pre-existing detached construction is independent of the
            // new live binding's adoption and family ownership.
            stage.add(speech_bubble_default(Some(&content))?)?
        } else {
            let content = stage.arena_mut().add(content);
            let bubble = SpeechBubble::new().add_to(stage.arena_mut(), Some(content))?;
            assert_eq!(bubble.content(), Some(content));
            let root = bubble.mobject();
            stage.arena_mut().add_to_scene(root)?;
            root
        };
        stage.wait(0.25)?;
        stage.play(
            root.animate()
                .set_anim_args(AnimateArgs {
                    run_time: Some(0.25),
                    rate_func: Some(fmn::core::rate::linear),
                    ..AnimateArgs::default()
                })?
                .shift([0.5, 0.25, 0.0])?,
        )?;
        stage.wait(0.25)?;
        Ok(())
    }
}

fn render(detached: bool, threads: u32, slots: usize) -> Result<Vec<Vec<u8>>, String> {
    let fs = Arc::new(VirtualFs::new());
    let mut options = RenderOptions::new("/bubbles").map_err(|error| error.to_string())?;
    options.config.camera.resolution = (96, 64);
    options.config.camera.fps = 8;
    options.config.sizes.frame_height = 4.0;
    options.config.determinism.mode = DeterminismMode::Certified;
    options.config.render.threads = ThreadPolicy::Fixed(threads);
    options.frames_in_flight = slots;
    let report = render_with_fs(&mut SpeechScene { detached }, options, fs.clone())
        .map_err(|error| error.to_string())?;
    if report.artifact.frame_count != 6 {
        return Err(format!(
            "bubble scene emitted {} frames, expected 6",
            report.artifact.frame_count
        ));
    }
    let pipeline = report
        .frame_pipeline
        .ok_or("bubble pipeline report is absent")?;
    if (
        pipeline.submitted,
        pipeline.emitted,
        pipeline.outstanding_slots,
    ) != (6, 6, 0)
        || pipeline.max_in_flight > slots
    {
        return Err(format!(
            "bubble pipeline did not join cleanly: {pipeline:?}"
        ));
    }
    (0..6)
        .map(|index| {
            fs.read(&Path::new("/bubbles").join(format!("frame_{index:06}.png")))
                .map_err(|error| error.to_string())
        })
        .collect()
}

/// Return the number of complete certified PNG comparisons and a sample frame.
pub fn verify() -> Result<(u64, Vec<u8>), String> {
    let expected = render(true, 1, 1)?;
    if !expected.windows(2).any(|frames| frames[0] != frames[1]) {
        return Err("bubble motion produced no visible change".into());
    }
    let mut comparisons = 0;
    for (threads, slots) in [(1, 1), (4, 2), (16, 4)] {
        let actual = render(false, threads, slots)?;
        if actual != expected {
            return Err(format!(
                "live bubble body/content differ from detached composition at {threads} threads"
            ));
        }
        comparisons += actual.len() as u64;
    }
    Ok((comparisons, expected[3].clone()))
}
