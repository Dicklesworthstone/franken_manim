//! Offline camera selection is an input capability, not a completion-order guess.
//!
//! Vector-only inputs retain their original affine path. A compiled artifact
//! with camera-only draws uses one fixed camera for the entire generation,
//! including frames before/after a 3D object appears. The exporter did not
//! record a camera-rig binding in FMTL/1; no tracker family is guessed as one.

use super::{CliError, NativeRenderInput};
use fmn_render::{Camera, CameraConfig, CameraFrame};

pub(super) fn for_input(
    input: &NativeRenderInput,
    config: &fmn_config::Config,
) -> Result<Option<Camera>, CliError> {
    let needs_camera = match input {
        NativeRenderInput::Builtin { names } => names.iter().any(|name| builtin(name).is_some()),
        NativeRenderInput::Compiled { bundle, .. } => bundle.requires_camera(),
    };
    if !needs_camera {
        return Ok(None);
    }
    if config.render.engine != fmn_config::config::Engine::Cpu {
        return Err(CliError::new(
            "capability",
            "mixed camera content requires the CPU renderer; no accelerator request is silently substituted",
        ));
    }
    if config.render.aa != fmn_core::AaPolicy::Adaptive {
        return Err(CliError::new(
            "capability",
            "the camera route supports adaptive coverage; forced affine SSAA is not silently applied to perspective content",
        ));
    }
    camera(config).map(Some)
}

pub(super) fn camera(config: &fmn_config::Config) -> Result<Camera, CliError> {
    let frame_config = super::resolved_frame_config(config)?;
    let (width, height) = config.camera.resolution;
    let mut frame = CameraFrame::default();
    frame
        .set_shape([
            config.sizes.frame_height * f64::from(width) / f64::from(height),
            config.sizes.frame_height,
        ])
        .map_err(|error| CliError::new("config", error.to_string()))?;
    Camera::new(CameraConfig {
        resolution: (width, height),
        fps: config.camera.fps,
        background: frame_config.background,
        frame,
        ..CameraConfig::default()
    })
    .map_err(|error| CliError::new("config", error.to_string()))
}

/// Content descriptor for the actual perspective kernel and fixed camera.
/// An implementation revision counter cannot stand in for these pose bits.
pub(super) fn descriptor(camera: &Camera) -> Vec<u8> {
    let mut bytes = b"fmn.cli.fixed-camera.v1\0".to_vec();
    let (width, height) = camera.pixel_shape();
    bytes.extend_from_slice(&width.to_le_bytes());
    bytes.extend_from_slice(&height.to_le_bytes());
    bytes.extend_from_slice(&camera.fps().to_le_bytes());
    bytes.push(camera.samples());
    let frame = camera.frame();
    let background = camera.background();
    for value in frame
        .center()
        .into_iter()
        .chain(frame.shape())
        .chain(frame.orientation())
        .chain([frame.field_of_view(), camera.max_allowable_norm()])
        .chain(camera.light_source_position())
        .chain([background.r, background.g, background.b, background.a])
    {
        bytes.extend_from_slice(&value.to_bits().to_le_bytes());
    }
    bytes
}

// Beside, not inside, the digest-pinned 25-scene primitive corpus.
pub(super) const CAMERA_SCENE_NAMES: &[&str] = &[
    "surface_cube.v1",
    "dot_cloud_depth.v1",
    "image_quad.v1",
    "mixed_camera.v1",
];

pub(super) fn builtin(name: &str) -> Option<CameraScene> {
    CAMERA_SCENE_NAMES
        .iter()
        .copied()
        .find(|candidate| *candidate == name)
        .map(|name| CameraScene { name })
}

pub(super) struct CameraScene {
    name: &'static str,
}

impl fmn::SceneConstruct for CameraScene {
    fn name(&self) -> &str {
        self.name
    }

    fn construct(&mut self, stage: &mut fmn::Stage<'_>) -> fmn::Result<()> {
        use fmn::prelude::*;
        use fmn_library::{DotCloud, ImageMobject};
        let mixed = self.name == "mixed_camera.v1";
        let mut moving = None;
        if mixed || self.name == "surface_cube.v1" {
            let cube = stage.add(Cube::new(1.8).color(BLUE))?;
            stage.rotate(cube, 0.45, [1.0, 1.0, 0.0], Some(ORIGIN), None);
            stage.shift(cube, if mixed { [-1.8, 0.0, 0.0] } else { ORIGIN });
            moving = Some(cube);
        }
        if mixed || self.name == "dot_cloud_depth.v1" {
            let dots = stage.add(
                DotCloud::new([[-0.55, -0.5, -0.6], [0.0, 0.5, 0.4], [0.55, -0.25, 0.8]])
                    .colored(YELLOW, 1.0)
                    .with_radius(0.24)
                    .make_3d(),
            )?;
            moving.get_or_insert(dots);
        }
        if mixed || self.name == "image_quad.v1" {
            let image = ImageMobject::from_rgba8(
                2,
                2,
                vec![
                    255, 0, 0, 255, 0, 255, 0, 255, 0, 0, 255, 255, 255, 255, 255, 255,
                ],
            )
            .map_err(|error| {
                SceneError::Integration(fmn_scene::IntegrationError::new(
                    "image",
                    error.to_string(),
                ))
            })?;
            let image = stage.add(image.with_height(1.8))?;
            stage.shift(image, if mixed { [1.8, 0.0, 0.0] } else { ORIGIN });
            moving.get_or_insert(image);
        }
        let marker = stage.add(Circle::new().radius(0.14).color(WHITE))?;
        stage.set_fill(marker, Some(WHITE), Some(1.0), None, true);
        stage.shift(marker, [0.0, 1.5, 0.0]);
        if let Some(mob) = moving {
            stage.play(
                mob.animate()
                    .set_anim_args(AnimateArgs {
                        run_time: Some(0.25),
                        rate_func: Some(fmn::core::rate::linear),
                        ..AnimateArgs::default()
                    })?
                    .shift([0.5, 0.0, 0.0])?,
            )?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn camera_plan_names_its_real_cpu_kernel_and_budgets_retained_frames() {
        use fmn_runtime::{ExecutionEngine, OutputPixelFormat, RenderIntent};
        let fs = fmn_platform::fs::VirtualFs::new();
        let mut config = fmn_config::Config::resolve(&[], None).unwrap().config;
        config.camera.resolution = (64, 48);
        config.render.threads = fmn_config::config::ThreadPolicy::Fixed(1);
        let (affine, _, _) = super::super::derive_execution_plan(
            &fs,
            &config,
            RenderIntent::Offline,
            OutputPixelFormat::Rgba8,
        )
        .unwrap();
        let (camera, _, _) = super::super::derive_execution_plan_with_annex(
            &fs,
            &config,
            RenderIntent::Offline,
            OutputPixelFormat::Rgba8,
            super::super::MetalSelection::Offline,
            true,
        )
        .unwrap();
        assert_eq!(affine.engine, ExecutionEngine::FastCpu);
        assert_eq!(camera.engine, ExecutionEngine::CertifiedCpu);
        assert_eq!(
            config.determinism.mode,
            fmn_config::config::DeterminismMode::Standard
        );
        assert!(camera.estimated_in_flight_bytes >= affine.estimated_in_flight_bytes + 64 * 48 * 8);
    }

    #[test]
    fn fixed_camera_binds_output_shape_pose_and_coverage_policy() {
        let mut config = fmn_config::Config::resolve(&[], None).unwrap().config;
        config.camera.resolution = (64, 48);
        config.camera.fps = 8;
        config.sizes.frame_height = 4.0;
        let mut camera = camera(&config).unwrap();
        assert_eq!(camera.frame().shape(), [16.0 / 3.0, 4.0]);
        assert_eq!(camera.fps(), 8);
        let before = descriptor(&camera);
        camera.frame_mut().set_center([1.0, 0.0, 0.0]).unwrap();
        assert_ne!(descriptor(&camera), before);
        let input = NativeRenderInput::Builtin {
            names: vec!["surface_cube.v1".into()],
        };
        config.render.aa = fmn_core::AaPolicy::Ssaa2x;
        assert!(for_input(&input, &config).is_err());
        config.render.aa = fmn_core::AaPolicy::Adaptive;
        config.render.engine = fmn_config::config::Engine::Metal;
        assert!(for_input(&input, &config).is_err());
        let affine = NativeRenderInput::Builtin {
            names: vec!["circle_shift.v1".into()],
        };
        assert!(for_input(&affine, &config).unwrap().is_none());
    }

    #[test]
    fn camera_captures_use_every_render_team_and_match_serial_pixels() {
        use super::super::{NativeFrameFormat, RenderSink, RenderTarget};
        use fmn::mobject::{Mobject, Stage};
        use fmn_codec::{PngLimits, decode_png};
        use fmn_core::rng::RngRoot;
        use fmn_frame::convert::rgba16f_to_rgba8;
        use fmn_frame::{FrameBuffer, FrameLayout, PixelFormat};
        use fmn_platform::fs::{FileSystem, VirtualFs};
        use fmn_platform::topology::HardwareTopology;
        use fmn_render::{
            EngineIdentity, RetainedFrameRenderer, RetainedFrameRendererConfig, Tiling,
        };
        use fmn_runtime::{
            ExecutionPlan, OutputPixelFormat, PlanRequest, RenderIntent, SurfaceSpec,
        };
        use std::path::{Path, PathBuf};
        use std::sync::Arc;

        let mut config = fmn_config::Config::resolve(&[], None).unwrap().config;
        config.camera.resolution = (32, 24);
        config.camera.fps = 8;
        let camera = camera(&config).unwrap();
        let mut surface = SurfaceSpec::lumen(32, 24);
        surface.working_bytes_per_pixel += 16;
        let plan = ExecutionPlan::derive(
            PlanRequest::certified(RenderIntent::Offline, surface, OutputPixelFormat::Rgba8)
                .with_max_frames_in_flight(4),
            &HardwareTopology::from_group_sizes(&[2, 2]).unwrap(),
            None,
        )
        .unwrap();
        assert_eq!(plan.render_teams.len(), 2);
        let mut stage = Stage::new();
        let mob = stage.add(Mobject::from(fmn_library::Cube::new(1.4)));
        stage.add_to_scene(mob).unwrap();
        let mut timeline = fmn::animation::Timeline::new(8).unwrap();
        timeline.wait(1.0).unwrap();
        let bytes = fmn_scene::export_timeline_bundle(timeline, &mut stage, &RngRoot::from_seed(0))
            .unwrap();
        let shared = fmn_scene::timeline_bundle::SharedTimelineBundle::from_bytes(&bytes).unwrap();
        let mut serial = RetainedFrameRenderer::new(RetainedFrameRendererConfig {
            frame: super::super::resolved_frame_config(&config).unwrap(),
            tiling: Tiling {
                macro_tile: plan.macro_tile,
                fine_tile: plan.fine_tile,
            },
            engine: EngineIdentity::certified(),
            threads: 1,
        })
        .unwrap();
        serial.render_with_camera(&stage, &camera).unwrap();
        let mut expected =
            FrameBuffer::new(FrameLayout::tight(PixelFormat::Rgba8, 32, 24).unwrap());
        rgba16f_to_rgba8(serial.frame(), &mut expected).unwrap();
        for compiled in [false, true] {
            let fs = Arc::new(VirtualFs::new());
            let mut sink = RenderSink::new_with_camera(
                fs.clone(),
                &config,
                &plan,
                &RenderTarget::Native(NativeFrameFormat::PngSequence),
                PathBuf::from("/frames"),
                Some(camera.clone()),
            )
            .unwrap();
            for index in 0..8 {
                if compiled {
                    sink.render_compiled(shared.frame_job(index).unwrap())
                        .unwrap();
                } else {
                    sink.render_stage(&stage, u64::from(index)).unwrap();
                }
            }
            let report = sink.finish().unwrap();
            assert_eq!(
                report.backend.route,
                if compiled {
                    "compiled-camera-cpu"
                } else {
                    "camera-cpu"
                }
            );
            assert!(report.backend.journal.ends_with(&descriptor(&camera)));
            let stats = report.backend.pipeline.unwrap();
            assert_eq!(
                (stats.submitted, stats.emitted, stats.outstanding_slots),
                (8, 8, 0)
            );
            assert_eq!(stats.render_team_frames.len(), 2);
            assert!(stats.render_team_frames.iter().all(|n| *n > 0));
            for index in 0..8 {
                let png = fs
                    .read(Path::new(&format!("/frames/frame_{index:06}.png")))
                    .unwrap();
                assert_eq!(
                    decode_png(&png, &PngLimits::default()).unwrap().rgba,
                    expected.as_bytes()
                );
            }
        }
    }
}
