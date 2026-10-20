//! Public native vector output: final scene state, camera rigs and refusals.

use std::cell::Cell;
use std::path::Path;
use std::rc::Rc;
use std::sync::Arc;

use fmn::prelude::*;
use fmn::{SvgRenderError, SvgRenderOptions, render_svg_camera_with_fs, render_svg_with_fs};
use fmn_config::config::ThreadPolicy;
use fmn_geom::{Paint, SvgDocument};
use fmn_platform::fs::{FileSystem, VirtualFs};
use fmn_render::{Camera, CameraConfig};
use fmn_scene::CameraRig;

fn options(path: &str) -> SvgRenderOptions {
    let mut options = SvgRenderOptions::new(path).unwrap();
    options.config.camera.resolution = (160, 90);
    options.config.camera.fps = 8;
    options.config.sizes.frame_height = 8.0;
    options.config.camera.background_color = "#123456".into();
    options.config.camera.background_opacity = 0.5;
    options.typeset_cache = false;
    options
}

fn center(document: &SvgDocument, shape: usize) -> [f64; 2] {
    let points = document.shapes[shape].path.points();
    [0, 1].map(|axis| {
        let low = points.iter().map(|p| p[axis]).fold(f64::INFINITY, f64::min);
        let high = points
            .iter()
            .map(|p| p[axis])
            .fold(f64::NEG_INFINITY, f64::max);
        (low + high) / 2.0
    })
}

struct Updated(Rc<Cell<u32>>);

impl SceneConstruct for Updated {
    fn construct(&mut self, stage: &mut Stage<'_>) -> fmn::Result<()> {
        let square = stage.add(Square::new())?;
        stage.set_fill(square, Some(BLUE), Some(0.7), None, false);
        stage.set_stroke(
            square,
            Some(YELLOW),
            Some(4.0),
            Some(0.6),
            Some(true),
            false,
        );
        let calls = self.0.clone();
        stage.arena_mut().add_dt_updater(
            square,
            move |arena, mob, dt| {
                if dt > 0.0 {
                    calls.set(calls.get() + 1);
                }
                arena.shift(mob, [dt, 0.0, 0.0]);
            },
            false,
        )?;
        stage.wait(0.5)?;
        // This edit follows the last captured frame. A final SVG must include
        // it, rather than publish the last packet received by the sink.
        stage.shift(square, [1.5, 0.0, 0.0]);
        Ok(())
    }
}

#[test]
fn native_svg_runs_updaters_and_publishes_the_completed_state_once() {
    let fs = Arc::new(VirtualFs::new());
    let mut baseline = None;
    for threads in [1, 4] {
        let path = format!("/scene-{threads}.svg");
        let mut request = options(&path);
        request.config.render.threads = ThreadPolicy::Fixed(threads);
        let camera = Camera::new(request.camera_config().unwrap()).unwrap();
        let calls = Rc::new(Cell::new(0));
        let report = render_svg_with_fs(&mut Updated(calls.clone()), request, fs.clone()).unwrap();
        assert_eq!(calls.get(), 4);
        assert_eq!(
            (report.scene.time.frames(), report.scene.play_count),
            (4, 1)
        );
        let bytes = fs.read(Path::new(&path)).unwrap();
        assert_eq!(report.artifact.bytes, bytes.len() as u64);
        let document = SvgDocument::parse(&bytes).unwrap();
        assert_eq!(document.shapes.len(), 3, "background, stroke-behind, fill");
        assert_eq!(document.shapes[0].style.fill_opacity, 0.5);
        assert_eq!(
            document.shapes[0].style.fill,
            Some(Paint::Color(Srgb::from_hex("#123456").unwrap()))
        );
        assert!(document.shapes[1].style.fill.is_none());
        assert!(document.shapes[1].style.stroke.is_some());
        assert!(document.shapes[2].style.fill.is_some());
        assert!(document.shapes[2].style.stroke.is_none());
        let expected = camera
            .project([2.0, 0.0, 0.0], 0.0)
            .pixel((160, 90))
            .unwrap();
        let actual = center(&document, 2);
        assert!((actual[0] - expected[0]).abs() < 1e-5);
        assert!((actual[1] - expected[1]).abs() < 1e-5);
        if let Some(baseline) = &baseline {
            assert_eq!(&bytes, baseline);
        } else {
            baseline = Some(bytes);
        }
    }
}

struct CameraMove {
    rig: CameraRig,
    base: CameraConfig,
}

impl SceneConstruct for CameraMove {
    fn construct(&mut self, stage: &mut Stage<'_>) -> fmn::Result<()> {
        let world = stage.add(Square::new())?;
        stage.shift(world, [2.0, 0.0, 0.0]);
        let overlay = stage.add(Square::new().side_length(0.5))?;
        stage
            .arena_mut()
            .uniforms_mut(overlay)
            .unwrap()
            .is_fixed_in_frame = 1.0;
        let mut target = self.base.clone();
        target.frame.set_center([1.0, 0.0, 0.0])?;
        target.frame.set_width(8.0)?;
        let animation = self.rig.animate_to(stage.scene_mut(), &target)?;
        stage.play_prepared_with(
            vec![Box::new(animation)],
            PlayOverrides {
                run_time: Some(0.25),
                rate_func: Some(RateFunc::linear()),
                ..Default::default()
            },
        )?;
        // Final camera edits must be read from the rig after the last packet.
        stage
            .arena_mut()
            .set_tracker_value(self.rig.center()[0], 2.0)?;
        Ok(())
    }
}

#[test]
fn native_svg_camera_samples_final_rig_and_keeps_fixed_overlay() {
    let fs = Arc::new(VirtualFs::new());
    let report = render_svg_camera_with_fs(
        |scene, base| {
            let rig = CameraRig::new(scene, base)?;
            Ok((
                CameraMove {
                    rig,
                    base: base.clone(),
                },
                rig,
            ))
        },
        options("/camera.svg"),
        fs.clone(),
    )
    .unwrap();
    assert_eq!(report.scene.time.frames(), 2);
    let document = SvgDocument::parse(&fs.read(Path::new("/camera.svg")).unwrap()).unwrap();
    assert_eq!(document.shapes.len(), 3, "camera trackers carry no paint");
    for shape in [1, 2] {
        let actual = center(&document, shape);
        assert!((actual[0] - 80.0).abs() < 1e-5);
        assert!((actual[1] - 45.0).abs() < 1e-5);
    }
}

struct Refused(u8);

impl SceneConstruct for Refused {
    fn construct(&mut self, stage: &mut Stage<'_>) -> fmn::Result<()> {
        if self.0 == 0 {
            stage.add(Cube::new(1.0))?;
        } else {
            let square = stage.add(Square::new())?;
            match self.0 {
                1 => stage.arena_mut().uniforms_mut(square).unwrap().depth_test = true,
                2 => {
                    stage.arena_mut().get_mut(square).unwrap().buffer.write(
                        0,
                        "fill_rgba",
                        &[1.0; 4],
                    );
                }
                3 => {
                    let _ = stage.wait(1.0);
                }
                _ => return Err(SceneError::InvalidState("deliberate SVG scene refusal").into()),
            }
        }
        Ok(())
    }
}

#[test]
fn native_svg_refusals_never_publish_partial_documents() {
    let fs = Arc::new(VirtualFs::new());
    for reason in 0..5 {
        let path = format!("/refused-{reason}.svg");
        let mut request = options(&path);
        request.max_scene_frames = 2;
        let error = render_svg_with_fs(&mut Refused(reason), request, fs.clone()).unwrap_err();
        match reason {
            0..=2 => assert!(matches!(error, SvgRenderError::Projection(_))),
            3 => assert!(matches!(
                error,
                SvgRenderError::InvalidOptions("SVG scene-frame limit exceeded")
            )),
            _ => assert!(matches!(error, SvgRenderError::Scene(_))),
        }
        assert!(!fs.exists(Path::new(&path)));
    }
    let mut request = options("/small.svg");
    request.max_output_bytes = 1;
    let error =
        render_svg_with_fs(&mut Updated(Rc::new(Cell::new(0))), request, fs.clone()).unwrap_err();
    assert!(matches!(error, SvgRenderError::Publication(_)));
    assert!(!fs.exists(Path::new("/small.svg")));
}

#[test]
fn native_svg_existing_destination_and_invalid_camera_refuse_before_scene_code() {
    struct Counted(Rc<Cell<u32>>);
    impl SceneConstruct for Counted {
        fn construct(&mut self, _: &mut Stage<'_>) -> fmn::Result<()> {
            self.0.set(self.0.get() + 1);
            Ok(())
        }
    }
    let fs = Arc::new(VirtualFs::new());
    fs.insert("/existing.svg", b"retained user artifact".to_vec());
    let calls = Rc::new(Cell::new(0));
    assert!(
        render_svg_with_fs(
            &mut Counted(calls.clone()),
            options("/existing.svg"),
            fs.clone()
        )
        .is_err()
    );
    assert_eq!(
        fs.read(Path::new("/existing.svg")).unwrap(),
        b"retained user artifact"
    );
    let mut request = options("/mismatch.svg");
    let mut camera = request.camera_config().unwrap();
    camera.resolution = (320, 180);
    request.camera = Some(camera);
    assert!(render_svg_with_fs(&mut Counted(calls.clone()), request, fs.clone()).is_err());
    assert_eq!(calls.get(), 0);
    assert!(!fs.exists(Path::new("/mismatch.svg")));
}

#[test]
fn native_svg_host_path_is_frozen_before_scene_code() {
    const CHILD: &str = "FMN_SVG_CWD_TEST_ROOT";
    if let Some(root) = std::env::var_os(CHILD) {
        struct ChangesDirectory(std::path::PathBuf);
        impl SceneConstruct for ChangesDirectory {
            fn construct(&mut self, stage: &mut Stage<'_>) -> fmn::Result<()> {
                std::env::set_current_dir(&self.0).unwrap();
                stage.add(Square::new())?;
                Ok(())
            }
        }
        let root = std::path::PathBuf::from(root);
        let moved = root.join("moved");
        for camera_rig in [false, true] {
            std::env::set_current_dir(&root).unwrap();
            let leaf = if camera_rig {
                "camera.svg"
            } else {
                "scene.svg"
            };
            let report = if camera_rig {
                fmn::render_svg_camera(
                    |scene, base| {
                        let rig = CameraRig::new(scene, base)?;
                        // Freeze must precede even the factory, not just construct.
                        std::env::set_current_dir(&moved).unwrap();
                        Ok((ChangesDirectory(moved.clone()), rig))
                    },
                    options(leaf),
                )
                .unwrap()
            } else {
                fmn::render_svg(&mut ChangesDirectory(moved.clone()), options(leaf)).unwrap()
            };
            assert_eq!(report.artifact.path, root.join(leaf));
            assert!(root.join(leaf).is_file());
            assert!(!moved.join(leaf).exists());
        }
        return;
    }
    // The cwd-changing scene runs in an isolated test process; parallel tests
    // never observe its working-directory changes.
    let root = std::env::temp_dir().join(format!("fmn-svg-cwd-{}", std::process::id()));
    std::fs::create_dir(&root).unwrap();
    std::fs::create_dir(root.join("moved")).unwrap();
    let result = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "native_svg_host_path_is_frozen_before_scene_code",
            "--nocapture",
        ])
        .env(CHILD, &root)
        .current_dir(&root)
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&result.stdout),
        String::from_utf8_lossy(&result.stderr)
    );
}
