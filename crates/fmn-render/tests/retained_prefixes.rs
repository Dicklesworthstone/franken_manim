//! Public production-route witnesses for exact retained prefix composition.
//!
//! This small integration binary also permits running the raster contract
//! independently of the renderer's much larger internal unit-test corpus.

use fmn_core::color::LinearRgba;
use fmn_frame::FrameBuffer;
use fmn_geom::quadpath::QuadPath;
use fmn_mobject::{Mob, Mobject, RecordBuffer, RecordSchema, Stage};
use fmn_render::{
    AaPolicy, AaStats, Binning, EngineIdentity, FrameArena, FrameConfig, FrameJob, MonoTable,
    PixelTileCache, PrefixCacheStats, RasterTeam, RenderPlan, RetainedFrameRendererConfig,
    ScreenMap, Tiling, VectorFrameCompiler, Viewport,
};

fn rectangle(x0: f64, y0: f64, x1: f64, y1: f64) -> [[f64; 3]; 9] {
    [
        [x0, y0, 0.0],
        [(x0 + x1) / 2.0, y0, 0.0],
        [x1, y0, 0.0],
        [x1, (y0 + y1) / 2.0, 0.0],
        [x1, y1, 0.0],
        [(x0 + x1) / 2.0, y1, 0.0],
        [x0, y1, 0.0],
        [x0, (y0 + y1) / 2.0, 0.0],
        [x0, y0, 0.0],
    ]
}

fn filled(points: &[[f64; 3]], rgba: [f32; 4]) -> Mobject {
    let mut buffer = RecordBuffer::new(RecordSchema::vmobject(), points.len()).unwrap();
    for (index, point) in points.iter().enumerate() {
        #[allow(clippy::cast_possible_truncation)]
        let point = point.map(|coordinate| coordinate as f32);
        assert!(buffer.write(index, "point", &point));
        assert!(buffer.write(index, "fill_rgba", &rgba));
        assert!(buffer.write(index, "stroke_rgba", &[0.0; 4]));
        assert!(buffer.write(index, "stroke_width", &[0.0]));
    }
    Mobject::from_buffer(buffer)
}

fn fill(stage: &mut Stage, mob: Mob, rgba: [f32; 4]) {
    let buffer = &mut stage.get_mut(mob).unwrap().buffer;
    for row in 0..buffer.len() {
        assert!(buffer.write(row, "fill_rgba", &rgba));
    }
}

fn scene() -> (Stage, Mob, Mob) {
    let mut stage = Stage::new();
    let full = rectangle(-8.0, -8.0, 40.0, 32.0);
    let background = stage.add(filled(&full, [0.14, 0.32, 0.73, 0.37]));
    stage.add_to_scene(background).unwrap();

    // Two separate subpixel stems contribute four boundary sheets in one
    // column. A single stem elsewhere requires 2x, so both AA thresholds must
    // survive a restored prefix. Their ends stay off pixel-row boundaries.
    let mut dense = QuadPath::new();
    dense
        .add_subpath(&rectangle(8.1, 2.25, 8.2, 21.75))
        .unwrap();
    dense
        .add_subpath(&rectangle(8.5, 2.25, 8.6, 21.75))
        .unwrap();
    let four_edges = stage.add(filled(dense.points(), [0.78, 0.19, 0.43, 0.41]));
    let two_edges = stage.add(filled(
        &rectangle(20.1, 2.25, 20.2, 21.75),
        [0.23, 0.71, 0.19, 0.53],
    ));
    stage.add_to_scene(four_edges).unwrap();
    stage.add_to_scene(two_edges).unwrap();

    // The foreground reaches every tile; changing its opacity dirties every
    // output pixel without altering the static prefix's classification.
    let foreground = stage.add(filled(&full, [0.17, 0.51, 0.93, 0.23]));
    stage.add_to_scene(foreground).unwrap();
    (stage, background, foreground)
}

fn config(engine: EngineIdentity, aa: AaPolicy) -> RetainedFrameRendererConfig {
    RetainedFrameRendererConfig {
        frame: FrameConfig::new(
            Viewport {
                width: 32,
                height: 24,
            },
            ScreenMap {
                scale: 1.0,
                origin: [0.0, 0.0],
                y_up: false,
            },
            LinearRgba {
                r: 0.04,
                g: 0.04,
                b: 0.06,
                a: 0.63,
            },
        )
        .with_aa_policy(aa),
        tiling: Tiling {
            macro_tile: 32,
            fine_tile: 8,
        },
        engine,
        threads: 1,
    }
}

fn uncached(stage: &Stage, config: RetainedFrameRendererConfig) -> (Vec<u8>, AaStats) {
    // Rebuild all derived tables from authoritative records. This oracle does
    // not use frozen captures, a retained renderer, a tile cache or prefixes.
    let mut plan = RenderPlan::new();
    plan.sync(stage, 0).unwrap();
    let mono = MonoTable::build(&plan, config.frame.map).unwrap();
    let mut binning = Binning::build(
        &plan,
        config.frame.viewport,
        config.tiling,
        config.frame.map,
    )
    .unwrap();
    binning.prune_occluded(&plan).unwrap();
    let (pixels, stats) =
        FrameJob::with_identity(&plan, &mono, &binning, config.frame, config.engine)
            .unwrap()
            .render_with_stats(1)
            .unwrap();
    (pixels.as_bytes().to_vec(), stats)
}

#[test]
fn retained_prefixes_match_uncached_pixels_sampling_and_replays() {
    for (engine, aa, eligible) in [
        (EngineIdentity::certified(), AaPolicy::Adaptive, true),
        // Certified normalizes even a forced-AA request to canonical sampling.
        (EngineIdentity::certified(), AaPolicy::Ssaa4x, true),
        (EngineIdentity::fast(), AaPolicy::Adaptive, true),
        (EngineIdentity::fast(), AaPolicy::Ssaa2x, false),
        (EngineIdentity::fast(), AaPolicy::Ssaa4x, false),
    ] {
        let config = config(engine, aa);
        let (mut stage, background, foreground) = scene();
        let mut compiler = VectorFrameCompiler::new(config).unwrap();
        let mut frames = Vec::new();
        let mut expected = Vec::new();
        for (index, alpha) in [0.23, 0.27, 0.31, 0.35, 0.39, 0.43, 0.47]
            .into_iter()
            .enumerate()
        {
            fill(&mut stage, foreground, [0.17, 0.51, 0.93, alpha]);
            if index == 4 {
                fill(&mut stage, background, [0.81, 0.24, 0.11, 0.49]);
            }
            expected.push(uncached(&stage, config));
            frames.push(compiler.capture(&stage, 0).unwrap());
        }
        assert!(
            expected.windows(2).all(|pair| pair[0].0 != pair[1].0),
            "every mutation must visibly change the independent oracle"
        );

        for threads in [1, 4, 16] {
            for budget in [0, 1, 64 * 1024] {
                let mut team = RasterTeam::new(threads).unwrap();
                let mut arena = FrameArena::new();
                let mut cache = PixelTileCache::with_budgets(1024 * 1024, budget);
                let mut pixels = FrameBuffer::new(config.frame.layout().unwrap());
                let mut resume = None;
                for (position, index) in [0, 1, 2, 3, 4, 5, 6, 6, 2].into_iter().enumerate() {
                    let stats = frames[index]
                        .render_cached_into_with_team(
                            &mut team,
                            &mut arena,
                            &mut cache,
                            &mut pixels,
                            resume,
                        )
                        .unwrap();
                    assert_eq!(
                        pixels.as_bytes(),
                        expected[index].0,
                        "engine={engine:?}, aa={aa:?}, threads={threads}, budget={budget}, \
                         position={position}"
                    );
                    assert!(cache.retained_prefix_bytes() <= budget);
                    assert_eq!(stats.prefix, cache.prefix_stats());
                    if position == 7 {
                        assert!(
                            stats.cache.hits > 0,
                            "identical frames reuse complete tiles"
                        );
                        assert_eq!(stats.prefix, PrefixCacheStats::default());
                    } else {
                        assert_eq!(stats.cache.hits, 0, "the foreground dirties every tile");
                        assert_eq!(
                            stats.raster, expected[index].1,
                            "prefix reuse changed the independent AA classifier"
                        );
                    }
                    if !eligible || budget <= 1 {
                        assert_eq!(stats.prefix, PrefixCacheStats::default());
                        assert_eq!(cache.retained_prefix_bytes(), 0);
                    } else if matches!(position, 2 | 3 | 6) {
                        assert!(stats.prefix.hits > 0, "dirty tiles must resume prefixes");
                        assert!(stats.prefix.commands_skipped > 0);
                        if !engine.engine.certifiable() {
                            assert!(stats.raster.ssaa2x_cells > 0, "retain 2x boundary counts");
                            assert!(stats.raster.ssaa4x_cells > 0, "retain 4x boundary counts");
                        }
                    } else if matches!(position, 1 | 5) {
                        assert!(stats.prefix.captures > 0, "retain the stable leading run");
                    } else if matches!(position, 0 | 4 | 8) {
                        assert_eq!(stats.prefix, PrefixCacheStats::default());
                    }
                    resume = Some(cache.serial());
                }
                assert_eq!(team.stats().frames_completed, 9);
            }
        }
    }
}
