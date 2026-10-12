//! Cached motion must render like a fresh world-space Chisel query each frame.
#![forbid(unsafe_code)]

use std::path::Path;
use std::sync::Arc;

use fmn::prelude::*;
use fmn::rendering::render_bundle_with_fs;
use fmn_anim::UpdateFromFunc;
use fmn_anim::animation::AnimState;
use fmn_config::config::{DeterminismMode, ThreadPolicy};
use fmn_geom::QuadPath;
use fmn_mobject::{Placement, Stage as Arena};
use fmn_platform::fs::{FileSystem, VirtualFs};
use fmn_scene::timeline_bundle::{BundleSegmentKind, TimelineBundle};

#[derive(Clone, Copy)]
enum Route {
    Cached,
    Fresh,
    QuickNegative,
    StaleNegative,
}

fn config() -> AnimConfig {
    AnimConfig {
        run_time: 1.0,
        rate_func: RateFunc::linear(),
        ..AnimConfig::default()
    }
}

// This oracle uses the preexisting public path and placement kernels but never
// MoveAlongPath or the retained path/table. It intentionally rebuilds each time.
struct ObservedPath {
    state: AnimState,
    path: Mob,
    route: Route,
    original: QuadPath,
}

impl Animation for ObservedPath {
    fn state(&self) -> &AnimState {
        &self.state
    }

    fn state_mut(&mut self) -> &mut AnimState {
        &mut self.state
    }

    fn interpolate(&mut self, stage: &mut Arena, alpha: f64) {
        let current = QuadPath::from_points(stage.get_points(self.path).unwrap()).unwrap();
        let alpha = self.state.config.rate_func.eval(alpha);
        let point = match self.route {
            Route::StaleNegative => self.original.point_from_proportion(alpha),
            Route::QuickNegative => current.quick_point_from_proportion(alpha),
            _ => current.point_from_proportion(alpha),
        }
        .unwrap();
        stage.move_to(self.state.mobject(), point, [0.0; 3]);
    }

    fn interpolate_submobject(&mut self, _: &mut Arena, _: &[Mob], _: f64) {}
}

struct FollowingScene {
    route: Route,
    deform: bool,
}

impl SceneConstruct for FollowingScene {
    fn construct(&mut self, stage: &mut Stage<'_>) -> fmn::Result<()> {
        let path = stage.add(Mobject::vector_from_points(&[
            [-2.0, -1.0, 0.0],
            [-1.9, -0.8, 0.0],
            [-1.75, -0.75, 0.0],
            [-0.5, 2.0, 0.0],
            [1.0, 0.0, 0.0],
            [2.0, -1.0, 0.0],
            [2.25, 1.0, 0.0],
        ]))?;
        stage.set_fill(path, None, Some(0.0), None, true);
        stage.set_stroke(path, Some(WHITE), Some(1.5), Some(1.0), None, true);
        let moving = stage.add(Circle::new().radius(0.18).color(BLUE))?;
        stage.set_fill(moving, Some(BLUE), Some(1.0), None, true);
        stage.set_stroke(moving, None, Some(0.0), None, None, true);
        let original = QuadPath::from_points(stage.get_points(path).unwrap()).unwrap();
        let motion: Box<dyn Animation> = match self.route {
            Route::Cached => Box::new(MoveAlongPath::new(moving, path).with_config(config())),
            route => Box::new(ObservedPath {
                state: AnimState::new(moving, config()),
                path,
                route,
                original,
            }),
        };
        if self.deform {
            // Runs before path following in each frame. A cached object-space
            // table or begin-only invalidation would give the wrong speed.
            let mut warp = UpdateFromFunc::new_alpha(path, |arena, path, alpha| {
                arena
                    .set_placement(
                        path,
                        Placement::new(
                            [
                                [1.0 - 0.2 * alpha, 0.25 * alpha, 0.0],
                                [0.0, 1.0 + 0.35 * alpha, 0.0],
                                [0.0, 0.0, 1.0],
                            ],
                            [0.2 * alpha, 0.0, 0.0],
                        ),
                    )
                    .unwrap();
            });
            warp.state_mut().config = config();
            let group = AnimationGroup::new(stage.arena_mut(), vec![Box::new(warp), motion])?;
            stage.play(group)?;
        } else {
            let group = AnimationGroup::new(stage.arena_mut(), vec![motion])?;
            stage.play(group)?;
        }
        Ok(())
    }
}

fn export_options() -> BundleExportOptions {
    let mut options = BundleExportOptions::new().unwrap();
    options.config.camera.fps = 8;
    options.config.camera.resolution = (64, 48);
    options.config.sizes.frame_height = 4.5;
    options.config.determinism.mode = DeterminismMode::Certified;
    options.config.render.threads = ThreadPolicy::Fixed(1);
    options
}

fn render_options(directory: &str, threads: u32) -> RenderOptions {
    let mut options = RenderOptions::new(directory).unwrap();
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

#[test]
fn cached_static_and_live_paths_match_uncached_pixels_at_all_worker_counts() {
    for deform in [false, true] {
        let fs = Arc::new(VirtualFs::new());
        let control = render_with_fs(
            &mut FollowingScene {
                route: Route::Fresh,
                deform,
            },
            render_options("/control", 1),
            fs.clone(),
        )
        .unwrap();
        assert_eq!(control.artifact.frame_count, 8);
        let expected = frames(&fs, "/control");
        assert!(expected.windows(2).all(|pair| pair[0] != pair[1]));
        let negative = if deform {
            Route::StaleNegative
        } else {
            Route::QuickNegative
        };
        render_with_fs(
            &mut FollowingScene {
                route: negative,
                deform,
            },
            render_options("/negative", 1),
            fs.clone(),
        )
        .unwrap();
        assert_ne!(frames(&fs, "/negative"), expected);
        for threads in [1, 4, 16] {
            let directory = format!("/actual-{threads}");
            let actual = render_with_fs(
                &mut FollowingScene {
                    route: Route::Cached,
                    deform,
                },
                render_options(&directory, threads),
                fs.clone(),
            )
            .unwrap();
            assert_eq!(actual.artifact.frame_count, 8);
            assert_eq!(frames(&fs, &directory), expected);
        }
    }
}

#[test]
fn cached_motion_exports_the_observed_frames_not_its_transient_path_table() {
    let fs = Arc::new(VirtualFs::new());
    for deform in [false, true] {
        let bytes = export_bundle_bytes(
            &mut FollowingScene {
                route: Route::Cached,
                deform,
            },
            export_options(),
        )
        .unwrap()
        .bundle
        .bytes;
        let reader = TimelineBundle::from_bytes(&bytes).unwrap();
        assert_eq!(reader.frame_count(), 8);
        assert_eq!(reader.segment_kind(0), Some(BundleSegmentKind::Stateful));
        let control = format!("/control-{deform}");
        render_with_fs(
            &mut FollowingScene {
                route: Route::Fresh,
                deform,
            },
            render_options(&control, 1),
            fs.clone(),
        )
        .unwrap();
        let expected = frames(&fs, &control);
        for threads in [1, 4, 16] {
            let directory = format!("/replay-{deform}-{threads}");
            let replay =
                render_bundle_with_fs(&bytes, render_options(&directory, threads), fs.clone())
                    .unwrap();
            assert_eq!(replay.artifact.frame_count, 8);
            assert_eq!(frames(&fs, &directory), expected);
        }
    }
}
