//! Public camera-rig authoring -> portable artifact -> native output, no sink glue.
use std::cell::Cell;
use std::path::Path;
use std::rc::Rc;
use std::sync::Arc;

use fmn::prelude::*;
use fmn::rendering::{CameraConfig, render_bundle_with_fs, render_camera_with_fs};
use fmn_config::config::{DeterminismMode, ThreadPolicy};
use fmn_platform::fs::{FileSystem, VirtualFs};
use fmn_scene::{CameraRig, TimelineBundle};

fn options() -> BundleExportOptions {
    let mut options = BundleExportOptions::new().unwrap();
    options.config.camera.fps = 8;
    options.config.camera.resolution = (48, 32);
    options.config.determinism.mode = DeterminismMode::Certified;
    options.config.render.threads = ThreadPolicy::Fixed(1);
    options
}

fn output(path: &str, threads: u32) -> RenderOptions {
    let mut result = RenderOptions::new(path).unwrap();
    result.config = options().config;
    result.config.render.threads = ThreadPolicy::Fixed(threads);
    result.frames_in_flight = 2;
    result
}

fn camera() -> CameraConfig {
    let mut result = output("/unused", 1).camera_config().unwrap();
    result.frame.set_width(5.0).unwrap();
    result.frame.set_orientation([1.0, 2.3, -4.7, 0.9]).unwrap();
    result.background = BLUE.to_linear(1.0);
    result
}

struct Orbit {
    rig: CameraRig,
    base: CameraConfig,
    calls: Rc<Cell<u32>>,
}
impl SceneConstruct for Orbit {
    fn construct(&mut self, stage: &mut Stage<'_>) -> fmn::Result<()> {
        self.calls.set(self.calls.get() + 1);
        stage.add(Cube::new(1.4).color(YELLOW))?;
        let mut target = self.base.clone();
        target.frame.set_orientation([0.1, 0.8, 0.2, 0.5]).unwrap();
        target.frame.set_center([0.25, -0.1, 0.0]).unwrap();
        target.frame.set_width(3.5).unwrap();
        target.frame.set_field_of_view(0.9).unwrap();
        target.light_source_position = [4.0, 3.0, 8.0];
        let animation = self.rig.animate_to(stage.scene_mut(), &target)?;
        stage.play_prepared_with(
            vec![Box::new(animation)],
            PlayOverrides {
                run_time: Some(0.5),
                rate_func: Some(RateFunc::linear()),
                ..PlayOverrides::default()
            },
        )?;
        stage.wait(0.125)?;
        Ok(())
    }
}

fn factory(
    calls: Rc<Cell<u32>>,
) -> impl FnOnce(&mut Scene, &CameraConfig) -> fmn::Result<(Orbit, CameraRig)> {
    move |scene, base| {
        let rig = CameraRig::new(scene, base)?;
        Ok((
            Orbit {
                rig,
                base: base.clone(),
                calls,
            },
            rig,
        ))
    }
}

#[test]
fn public_export_matches_direct_camera_render_and_replays_without_source_calls() {
    let fs = Arc::new(VirtualFs::new());
    let calls = Rc::new(Cell::new(0));
    let report = export_camera_bundle_with_fs(
        factory(calls.clone()),
        "/orbit.fmtl",
        camera(),
        options(),
        fs.clone(),
    )
    .unwrap();
    assert_eq!(calls.get(), 1);
    assert_eq!(
        (
            report.frame_count,
            report.scene.play_count,
            report.scene.time.frames()
        ),
        (5, 2, 5)
    );
    let bytes = fs.read(Path::new("/orbit.fmtl")).unwrap();
    assert_eq!(report.bytes, bytes.len());
    assert!(
        TimelineBundle::from_bytes(&bytes)
            .unwrap()
            .has_camera_track()
    );
    let repeat =
        export_camera_bundle_bytes(factory(Rc::new(Cell::new(0))), camera(), options()).unwrap();
    assert_eq!(repeat.bundle.bytes, bytes);
    assert_eq!(repeat.bundle.digest.to_hex(), report.digest);
    let mut direct = output("/direct", 1);
    // A direct camera render must agree with its export configuration
    // (`render_camera` refuses a mismatched background); the bundle instead
    // carries the camera's background in its track.
    direct.config.camera.background_color = BLUE.to_hex();
    direct.camera = Some(camera());
    render_camera_with_fs(factory(Rc::new(Cell::new(0))), direct, fs.clone()).unwrap();
    for threads in [1, 4] {
        let path = format!("/replay-{threads}");
        let replay = render_bundle_with_fs(&bytes, output(&path, threads), fs.clone()).unwrap();
        assert_eq!(replay.artifact.frame_count, 5);
        assert_eq!(replay.frame_pipeline.unwrap().outstanding_slots, 0);
        for index in 0..5 {
            let name = format!("frame_{index:06}.png");
            assert_eq!(
                fs.read(&Path::new(&path).join(&name)).unwrap(),
                fs.read(&Path::new("/direct").join(&name)).unwrap()
            );
        }
    }
    assert_ne!(
        fs.read(Path::new("/direct/frame_000000.png")).unwrap(),
        fs.read(Path::new("/direct/frame_000003.png")).unwrap()
    );
    assert_eq!(calls.get(), 1, "replay invoked the original scene");
}

struct Static;
impl SceneConstruct for Static {
    fn construct(&mut self, stage: &mut Stage<'_>) -> fmn::Result<()> {
        stage.add(Cube::new(1.0))?;
        Ok(())
    }
}
fn static_factory(scene: &mut Scene, base: &CameraConfig) -> fmn::Result<(Static, CameraRig)> {
    Ok((Static, CameraRig::new(scene, base)?))
}

#[test]
fn nondefault_static_camera_yields_one_still_without_clock_or_extra_updaters() {
    let calls = Rc::new(Cell::new(0));
    let seen = calls.clone();
    let result = export_camera_bundle_bytes(
        move |scene, base| {
            let rig = CameraRig::new(scene, base)?;
            scene.stage_mut().add_updater(
                rig.root(),
                move |_, _| {
                    seen.set(seen.get() + 1);
                },
                false,
            )?;
            Ok((Static, rig))
        },
        camera(),
        options(),
    )
    .unwrap();
    assert_eq!(result.bundle.frame_count, 1);
    assert_eq!(result.scene.time.frames(), 0);
    assert_eq!(result.scene.play_count, 0);
    assert_eq!(calls.get(), 0);
    let bundle = TimelineBundle::from_bytes(&result.bundle.bytes).unwrap();
    let (_, captured) = bundle.stage_at_with_camera(0, (48, 32)).unwrap();
    assert_eq!(captured.unwrap().background(), camera().background);
}

#[test]
fn preflight_errors_do_not_invoke_factory_and_foreign_rigs_do_not_run_program() {
    for choice in 0..5 {
        let mut opts = options();
        let mut cam = camera();
        match choice {
            0 => cam.fps = 30,
            1 => cam.resolution.0 = 0,
            2 => opts.limits.max_frames = 0,
            3 => opts.limits.max_capture_bytes = 0,
            _ => opts.max_output_bytes = 0,
        }
        let result = export_camera_bundle_bytes(
            |_, _| -> fmn::Result<(Static, CameraRig)> {
                panic!("invalid options reached the factory")
            },
            cam,
            opts,
        );
        assert!(result.is_err());
    }
    let calls = Rc::new(Cell::new(0));
    let seen = calls.clone();
    let result = export_camera_bundle_bytes(
        move |_, base| {
            let mut foreign = Scene::default();
            let rig = CameraRig::new(&mut foreign, base)?;
            Ok((
                Orbit {
                    rig,
                    base: base.clone(),
                    calls: seen,
                },
                rig,
            ))
        },
        camera(),
        options(),
    );
    assert!(result.is_err());
    assert_eq!(calls.get(), 0);
    let result = export_camera_bundle_bytes(
        |scene, base| {
            let rig = CameraRig::new(scene, base)?;
            scene.wait(Some(0.125), &mut NullSceneSink)?;
            Ok((Static, rig))
        },
        camera(),
        options(),
    );
    assert!(matches!(result, Err(BundleExportError::InvalidOptions(_))));
}

struct Failure(u8);
impl SceneConstruct for Failure {
    fn construct(&mut self, stage: &mut Stage<'_>) -> fmn::Result<()> {
        match self.0 {
            0 => {
                stage.wait(0.125)?;
                Err(SceneError::InvalidLifecycle("authored failure").into())
            }
            1 => {
                let _ = stage.wait(0.5);
                Ok(())
            }
            2 => {
                stage.scene_mut().add_sound("sound.wav", 0.0, None, None)?;
                Ok(())
            }
            3 => panic!("authored panic"),
            _ => {
                stage.wait(0.125)?;
                stage.end()
            }
        }
    }
}

#[test]
fn user_errors_swallowed_capture_failures_and_audio_do_not_publish() {
    for choice in 0..3 {
        let fs = Arc::new(VirtualFs::new());
        let mut opts = options();
        if choice == 1 {
            opts.limits.max_frames = 1;
        }
        let result = export_camera_bundle_with_fs(
            |scene, base| Ok((Failure(choice), CameraRig::new(scene, base)?)),
            "/failed.fmtl",
            camera(),
            opts,
            fs.clone(),
        );
        let error = result.unwrap_err();
        match choice {
            0 => assert!(matches!(
                error,
                BundleExportError::Scene(fmn::Error::Scene(SceneError::InvalidLifecycle(
                    "authored failure"
                )))
            )),
            1 => assert!(matches!(error, BundleExportError::Recording(_))),
            _ => assert!(matches!(error, BundleExportError::Capability(_))),
        }
        assert!(!fs.exists(Path::new("/failed.fmtl")));
    }
}

#[test]
fn output_caps_atomic_collisions_and_unwinding_preserve_destinations() {
    let fs = Arc::new(VirtualFs::new());
    let bytes = export_camera_bundle_bytes(static_factory, camera(), options())
        .unwrap()
        .bundle
        .bytes;
    let mut exact = options();
    exact.max_output_bytes = bytes.len();
    assert_eq!(
        export_camera_bundle_bytes(static_factory, camera(), exact)
            .unwrap()
            .bundle
            .bytes,
        bytes
    );
    let mut small = options();
    small.max_output_bytes = bytes.len() - 1;
    assert!(matches!(
        export_camera_bundle_with_fs(static_factory, "/small.fmtl", camera(), small, fs.clone()),
        Err(BundleExportError::OutputLimit { .. })
    ));
    assert!(!fs.exists(Path::new("/small.fmtl")));
    fs.write_atomic(Path::new("/occupied.fmtl"), b"original")
        .unwrap();
    assert!(matches!(
        export_camera_bundle_with_fs(
            static_factory,
            "/occupied.fmtl",
            camera(),
            options(),
            fs.clone()
        ),
        Err(BundleExportError::FileSystem(_))
    ));
    assert_eq!(fs.read(Path::new("/occupied.fmtl")).unwrap(), b"original");
    let panic = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        export_camera_bundle_with_fs(
            |scene, base| Ok((Failure(3), CameraRig::new(scene, base)?)),
            "/panic.fmtl",
            camera(),
            options(),
            fs.clone(),
        )
    }));
    assert!(panic.is_err());
    assert!(!fs.exists(Path::new("/panic.fmtl")));
}

#[test]
fn early_termination_keeps_completed_camera_frames() {
    let result = export_camera_bundle_bytes(
        |scene, base| Ok((Failure(4), CameraRig::new(scene, base)?)),
        camera(),
        options(),
    )
    .unwrap();
    assert!(result.scene.ended_early);
    assert_eq!(result.scene.time.frames(), 1);
    assert_eq!(result.bundle.frame_count, 1);
}

#[test]
fn camera_export_freezes_only_the_picture_at_each_owner_boundary() {
    struct Wait(CameraRig);
    impl SceneConstruct for Wait {
        fn construct(&mut self, stage: &mut Stage<'_>) -> fmn::Result<()> {
            stage.add(Cube::new(1.0))?;
            let image = fmn::mobject::ImageResource::rgba8(
                1,
                1,
                vec![255, 0, 255, 255],
                fmn::mobject::ImageColorSpace::Srgb,
                fmn::mobject::ImageSampler::default(),
            )
            .unwrap();
            let hidden_pixels = image.shared_pixels();
            stage
                .arena_mut()
                .add(Mobject::new().with_image_resource(image));
            stage.wait_until_frames(4, |_| false)?;
            assert_eq!(
                Arc::strong_count(&hidden_pixels),
                2,
                "camera export must not freeze detached arena history"
            );
            // No further capture: these edits must not change recorded cameras.
            stage
                .arena_mut()
                .set_tracker_value(self.0.center()[0], 100.0)?;
            stage
                .arena_mut()
                .set_tracker_value(self.0.light()[0], -100.0)?;
            Ok(())
        }
    }

    for compact in [false, true] {
        let count = Rc::new(Cell::new(0));
        let seen = Rc::clone(&count);
        let owner = std::thread::current().id();
        let base = camera();
        let mut options = options();
        if compact {
            options.storage = fmn::BundleStorage::Compact(Default::default());
        }
        let export = export_camera_bundle_bytes(
            move |scene, base| {
                let rig = CameraRig::new(scene, base)?;
                scene.stage_mut().add_dt_updater(
                    rig.root(),
                    move |stage, _, dt| {
                        assert_eq!(std::thread::current().id(), owner);
                        if dt > 0.0 {
                            seen.set(seen.get() + 1);
                        }
                        for (channel, speed) in [(rig.center()[0], 1.0), (rig.light()[0], 3.0)] {
                            let value = stage.tracker_value(channel).unwrap();
                            stage
                                .set_tracker_value(channel, value + speed * dt)
                                .unwrap();
                        }
                    },
                    false,
                )?;
                Ok((Wait(rig), rig))
            },
            base.clone(),
            options,
        )
        .unwrap();
        assert_eq!(count.get(), 4);
        assert_eq!(export.bundle.frame_count, 4);
        assert_eq!(export.scene.time.frames(), 4);
        let bundle = TimelineBundle::from_bytes(&export.bundle.bytes).unwrap();
        for index in [3, 0, 2, 1] {
            let (captured, camera) = bundle.stage_at_with_camera(index, (48, 32)).unwrap();
            let dt = f64::from(index + 1) / 8.0;
            let mut expected = base.clone();
            let center = base.frame.center();
            expected
                .frame
                .set_center([center[0] + dt, center[1], center[2]])
                .unwrap();
            expected.light_source_position[0] += 3.0 * dt;
            expected
                .frame
                .resize_to_aspect_ratio(48.0 / 32.0, false)
                .unwrap();
            assert_eq!(
                fmn_render::camera::CameraSample::capture(&camera.unwrap()).unwrap(),
                fmn_render::camera::CameraSample::capture(
                    &fmn_render::Camera::new(expected).unwrap()
                )
                .unwrap()
            );
            assert_eq!(captured.snapshot().image_resources().count(), 0);
        }
        assert_eq!(count.get(), 4, "replay must not invoke the camera updater");
    }
}
