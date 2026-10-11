//! Public Rust methods -> native frames -> callback-free FMTL playback.
use std::cell::Cell;
use std::path::Path;
use std::rc::Rc;
use std::sync::Arc;

use fmn::prelude::*;
use fmn::rendering::render_bundle_with_fs;
use fmn_config::config::{DeterminismMode, ThreadPolicy};
use fmn_platform::fs::{FileSystem, VirtualFs};
use fmn_scene::timeline_bundle::{BundleSegmentKind, TimelineBundle};

#[derive(Clone, Copy)]
enum Route {
    Deferred,
    IndependentTarget,
    EagerNegative,
}

struct NonlinearScene {
    route: Route,
    callbacks: Rc<Cell<usize>>,
}

fn short_linear() -> AnimConfig {
    AnimConfig {
        run_time: 0.5,
        rate_func: RateFunc::linear(),
        ..AnimConfig::default()
    }
}

impl SceneConstruct for NonlinearScene {
    fn construct(&mut self, stage: &mut Stage<'_>) -> fmn::Result<()> {
        let mob = stage.add(Circle::new().radius(0.35).color(BLUE))?;
        stage.set_fill(mob, Some(BLUE), Some(1.0), None, true);
        stage.shift(mob, [-1.0, 0.0, 0.0]);
        let owner = std::thread::current().id();
        let animations: Vec<Box<dyn Animation>> = match self.route {
            Route::Deferred => {
                let calls = Rc::clone(&self.callbacks);
                let first = ApplyMethod::new(mob, move |arena, target| {
                    assert_eq!(std::thread::current().id(), owner);
                    calls.set(calls.get() + 1);
                    arena.shift(target, [1.0, 0.0, 0.0]);
                    Ok(())
                })
                .with_config(short_linear());
                let calls = Rc::clone(&self.callbacks);
                let second = ApplyMethod::pointwise(mob, move |p| {
                    assert_eq!(std::thread::current().id(), owner);
                    calls.set(calls.get() + 1);
                    [p[0], p[1] + 0.6 * p[0] * p[0] + 0.5, p[2]]
                })
                .with_config(short_linear());
                assert_eq!(
                    self.callbacks.get(),
                    0,
                    "constructing targets cannot run callbacks"
                );
                vec![Box::new(first), Box::new(second)]
            }
            Route::IndependentTarget | Route::EagerNegative => {
                // Independent controls explicitly build both target states,
                // without using ApplyMethod or its point-mapping helper.
                let shifted = stage.copy_family(mob)?;
                stage.shift(shifted, [1.0, 0.0, 0.0]);
                let mapped = stage.copy_family(mob)?;
                if matches!(self.route, Route::IndependentTarget) {
                    stage.shift(mapped, [1.0, 0.0, 0.0]);
                }
                stage.apply_points_function(
                    mapped,
                    |p| [p[0], p[1] + 0.6 * p[0] * p[0] + 0.5, p[2]],
                    Some([0.0; 3]),
                    None,
                );
                vec![
                    Box::new(Transform::new(mob, shifted).with_config(short_linear())),
                    Box::new(Transform::new(mob, mapped).with_config(short_linear())),
                ]
            }
        };
        let sequence = Succession::new(stage.arena_mut(), animations)?;
        let report = stage.play(sequence)?;
        if matches!(self.route, Route::Deferred) {
            assert!(!report.purity.is_pure());
            assert!(self.callbacks.get() > 1);
        }
        Ok(())
    }
}

fn export_options() -> BundleExportOptions {
    let mut options = BundleExportOptions::new().unwrap();
    options.config.camera.fps = 8;
    options.config.camera.resolution = (64, 48);
    options.config.sizes.frame_height = 4.0;
    options.config.determinism.mode = DeterminismMode::Certified;
    options.config.render.threads = ThreadPolicy::Fixed(1);
    options
}

fn render_options(path: &str, threads: u32) -> RenderOptions {
    let mut options = RenderOptions::new(path).unwrap();
    options.format = RenderFormat::PngSequence;
    options.config = export_options().config;
    options.config.render.threads = ThreadPolicy::Fixed(threads);
    options
}

fn frames(fs: &VirtualFs, directory: &str) -> Vec<Vec<u8>> {
    (0..8)
        .map(|index| {
            fs.read(&Path::new(directory).join(format!("frame_{index:06}.png")))
                .unwrap()
        })
        .collect()
}

fn scene(route: Route) -> NonlinearScene {
    NonlinearScene {
        route,
        callbacks: Rc::new(Cell::new(0)),
    }
}

#[test]
fn deferred_native_methods_render_the_expected_noncommuting_sequence_at_all_worker_counts() {
    let fs = Arc::new(VirtualFs::new());
    let mut baseline = scene(Route::IndependentTarget);
    let expected =
        render_with_fs(&mut baseline, render_options("/expected", 1), fs.clone()).unwrap();
    assert_eq!(expected.artifact.frame_count, 8);
    let expected = frames(&fs, "/expected");
    assert_ne!(expected[0], expected[7], "the scene must visibly change");
    render_with_fs(
        &mut scene(Route::EagerNegative),
        render_options("/stale", 1),
        fs.clone(),
    )
    .unwrap();
    assert_ne!(
        expected,
        frames(&fs, "/stale"),
        "the oracle must detect eager target evaluation"
    );
    for threads in [1, 4, 16] {
        let directory = format!("/actual-{threads}");
        let actual = render_with_fs(
            &mut scene(Route::Deferred),
            render_options(&directory, threads),
            fs.clone(),
        )
        .unwrap();
        assert_eq!(actual.artifact.frame_count, 8);
        assert_eq!(frames(&fs, &directory), expected);
    }
}

#[test]
fn deferred_method_bundle_keeps_pixels_without_retaining_or_reinvoking_source_closures() {
    let fs = Arc::new(VirtualFs::new());
    let mut program = scene(Route::Deferred);
    let counter = Rc::clone(&program.callbacks);
    let exported = export_bundle_bytes(&mut program, export_options()).unwrap();
    assert_eq!(exported.bundle.frame_count, 8);
    let count = counter.get();
    assert!(count > 1);
    let reader = TimelineBundle::from_bytes(&exported.bundle.bytes).unwrap();
    assert_eq!(reader.segment_kind(0), Some(BundleSegmentKind::Stateful));
    drop(program);
    assert_eq!(
        Rc::strong_count(&counter),
        1,
        "neither the bundle nor reader may retain source closures"
    );
    render_with_fs(
        &mut scene(Route::IndependentTarget),
        render_options("/control", 1),
        fs.clone(),
    )
    .unwrap();
    let expected = frames(&fs, "/control");
    for threads in [1, 4, 16] {
        let directory = format!("/replay-{threads}");
        let replay = render_bundle_with_fs(
            &exported.bundle.bytes,
            render_options(&directory, threads),
            fs.clone(),
        )
        .unwrap();
        assert_eq!(replay.artifact.frame_count, 8);
        assert_eq!(frames(&fs, &directory), expected);
        assert_eq!(counter.get(), count);
    }
    let again = export_bundle_bytes(&mut scene(Route::Deferred), export_options()).unwrap();
    assert_eq!(again.bundle.bytes, exported.bundle.bytes);
}
