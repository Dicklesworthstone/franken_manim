//! A frozen output-camera baseline for the retained planar route.
//!
//! Camera construction and this admission policy share the same aspect fit.
//! The baseline is derived from output parameters, never from a live frame
//! that scene code may already have moved, rotated or zoomed.

use fmn_mobject::{ProgramKind, Stage};

use super::{
    Camera, CameraError, DEFAULT_FOVY, DEFAULT_FRAME_CENTER, DEFAULT_FRAME_SHAPE, IDENTITY_QUAT,
    Quat, Vec3, shape_at_output_aspect,
};

/// Immutable evidence that a centered planar screen map matches an output
/// camera's canonical frame. Create it once when configuring the output, then
/// reuse it for serial captures, readback and independently sampled workers.
///
/// This does not make an arbitrary live camera a new baseline. The expected
/// center, orientation, field of view and frame height remain the constructor
/// defaults; only the width follows the requested output aspect (BN-25).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PlanarCapturePolicy {
    resolution: (u32, u32),
    center: Vec3,
    shape: [f64; 2],
    orientation: Quat,
    field_of_view: f64,
    map_height_matches: bool,
}

impl PlanarCapturePolicy {
    /// Freeze the canonical camera frame at this output resolution.
    ///
    /// `map_frame_height` is the world-space height used by the retained
    /// screen map (`pixel_height / map_frame_height`). A different configured
    /// height keeps the camera route, preserving its existing admission rule.
    ///
    /// # Errors
    /// [`CameraError::ZeroDimension`] for an empty output.
    pub fn for_output(resolution: (u32, u32), map_frame_height: f64) -> Result<Self, CameraError> {
        let shape =
            shape_at_output_aspect(DEFAULT_FRAME_SHAPE, resolution)?.unwrap_or(DEFAULT_FRAME_SHAPE);
        Ok(Self {
            resolution,
            center: DEFAULT_FRAME_CENTER,
            shape,
            orientation: IDENTITY_QUAT,
            field_of_view: DEFAULT_FOVY,
            map_height_matches: (shape[1] - map_frame_height).abs() < 1e-12,
        })
    }

    /// Explain why the live capture needs projection instead of the retained
    /// planar renderer. Comparing the frame uses only the frozen scalar fields;
    /// it constructs no camera and allocates no camera-frame state per capture.
    #[must_use]
    pub fn refusal(&self, stage: &Stage, camera: &Camera) -> Option<&'static str> {
        self.camera_refusal(camera)
            .or_else(|| planar_stage_refusal(stage))
    }

    /// The projection-only half is also used when preparing a camera vector:
    /// canonical XY strokes must retain the affine renderer's width semantics.
    /// Geometry and camera-dependent style admission remain the caller's proof.
    pub(crate) fn camera_refusal(&self, camera: &Camera) -> Option<&'static str> {
        let frame = camera.frame();
        if !self.map_height_matches {
            Some("retained screen map differs from the output camera frame")
        } else if camera.pixel_shape() != self.resolution {
            Some("camera pixel shape differs from the configured output")
        } else if frame.center() != self.center
            || frame.shape() != self.shape
            || frame.orientation() != self.orientation
            || frame.field_of_view() != self.field_of_view
        {
            Some("camera frame differs from the default frame")
        } else {
            None
        }
    }
}

/// Explain why captured geometry needs Lumen's camera route. The same rule
/// guards planar FMTL export and ordinary portal captures, so pure offload
/// cannot change accepted geometry or guess that a whole animation stays 2D.
#[must_use]
pub fn planar_stage_refusal(stage: &Stage) -> Option<&'static str> {
    for item in stage.draw_plan().items() {
        let Some(entry) = stage.get(item.mob) else {
            return Some("stale drawable");
        };
        let uniforms = entry.uniforms();
        if item.key.program != ProgramKind::Vector
            || uniforms.depth_test
            || uniforms.shading != [0.0; 3]
            || uniforms.clip_planes != [[0.0; 4]; 4]
        {
            return Some(
                "depth, raster primitives, lighting or clip planes require a camera-bearing bundle",
            );
        }
        if uniforms.is_fixed_in_frame != 0.0 || uniforms.scale_stroke_with_zoom {
            return Some("fixed-in-frame or zoom-scaled strokes require a camera-bearing bundle");
        }
        let placement = entry.placement();
        if let Some(points) = entry.buffer.read_column("point")
            && points
                .as_chunks::<3>()
                .0
                .iter()
                .any(|point| placement.apply_point(point.map(f64::from))[2] != 0.0)
        {
            return Some("nonplanar geometry requires a camera-bearing bundle");
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        AA_VISUAL_BUDGET_V1_MAX_CHANNEL_ERROR, AA_VISUAL_BUDGET_V1_RMS_CHANNEL_ERROR, CameraConfig,
        EngineIdentity, FAST_VISUAL_BUDGET_V1_MAX_CHANNEL_ERROR,
        FAST_VISUAL_BUDGET_V1_RMS_CHANNEL_ERROR, FrameConfig, RetainedFrameRenderer,
        RetainedFrameRendererConfig, ScreenMap, Tiling, Viewport,
    };
    use fmn_core::constants::FRAME_HEIGHT;
    use fmn_frame::FrameBuffer;
    use fmn_geom::QuadPath;
    use fmn_mobject::{Mobject, Placement, RecordBuffer, RecordSchema};

    fn camera(resolution: (u32, u32)) -> Camera {
        Camera::new(CameraConfig {
            resolution,
            ..CameraConfig::default()
        })
        .unwrap()
    }

    fn vector_stage() -> (Stage, fmn_mobject::Mob) {
        let mut stage = Stage::new();
        let mob = stage.add(Mobject::vector_from_points(&[
            [-1.0, -1.0, 0.0],
            [0.0, 1.0, 0.0],
            [1.0, -1.0, 0.0],
        ]));
        stage.add_to_scene(mob).unwrap();
        (stage, mob)
    }

    fn raster_fixture() -> Stage {
        let mut stage = Stage::new();
        let rounded = fmn_geom::svg::SvgDocument::parse(
            br#"<svg xmlns="http://www.w3.org/2000/svg">
                <rect x="-2.2" y="-0.8" width="2.1" height="1.7" rx="0.4" ry="0.3"/>
            </svg>"#,
        )
        .unwrap();
        let circle =
            QuadPath::try_arc(0.0, std::f64::consts::TAU, 1.1, [1.05, 0.3, 0.0], Some(16)).unwrap();
        let open_curve =
            QuadPath::from_points(vec![[-2.0, -1.2, 0.0], [0.2, -2.3, 0.0], [2.4, -1.0, 0.0]])
                .unwrap();
        for (path, fill, stroke, width) in [
            (
                &rounded.shapes[0].path,
                [0.1, 0.5, 0.9, 0.8],
                [0.8, 0.9, 1.0, 0.7],
                1.5,
            ),
            (&circle, [0.8, 0.2, 0.1, 0.7], [0.9, 0.7, 0.2, 0.8], 2.0),
            (&open_curve, [0.0; 4], [0.2, 0.8, 0.5, 0.9], 2.5),
        ] {
            let points = path.points();
            let mut buffer = RecordBuffer::new(RecordSchema::vmobject(), points.len()).unwrap();
            let coordinates: Vec<f32> = points.iter().flatten().map(|&v| v as f32).collect();
            buffer.write_range("point", 0, &coordinates);
            buffer.write_range("fill_rgba", 0, &fill.repeat(points.len()));
            buffer.write_range("stroke_rgba", 0, &stroke.repeat(points.len()));
            buffer.write_range("stroke_width", 0, &vec![width; points.len()]);
            let mob = stage.add(Mobject::from_buffer(buffer));
            stage.add_to_scene(mob).unwrap();
        }
        stage
    }

    /// The same maximum/RMS linear-channel definitions and versioned limits
    /// used by the renderer's existing fast-versus-certified acceptance.
    fn visual_error(a: &FrameBuffer, b: &FrameBuffer) -> (f64, f64) {
        assert_eq!(a.layout(), b.layout());
        let mut maximum = 0.0f64;
        let mut squared = 0.0;
        let mut channels = 0u64;
        for y in 0..a.layout().height() as usize {
            for x in 0..a.layout().width() as usize {
                let offset = y * a.layout().stride(0) + x * 8;
                for channel in 0..4 {
                    let at = offset + channel * 2;
                    let read = |frame: &FrameBuffer| {
                        fmn_frame::half::f16_to_f64(u16::from_le_bytes([
                            frame.plane(0)[at],
                            frame.plane(0)[at + 1],
                        ]))
                    };
                    let error = (read(a) - read(b)).abs();
                    maximum = maximum.max(error);
                    squared += error * error;
                    channels += 1;
                }
            }
        }
        (maximum, (squared / channels as f64).sqrt())
    }

    #[test]
    fn planar_policy_admits_every_canonical_output_aspect() {
        let (stage, _) = vector_stage();
        for resolution in [
            (1920, 1080),
            (320, 180),
            (854, 480),
            (480, 854),
            (96, 64),
            (64, 96),
            (1, 1),
        ] {
            let camera = camera(resolution);
            let policy = PlanarCapturePolicy::for_output(resolution, FRAME_HEIGHT).unwrap();
            assert_eq!(policy.refusal(&stage, &camera), None, "{resolution:?}");
            assert_eq!(policy.shape, camera.frame().shape());
            assert_eq!(policy.shape[1], FRAME_HEIGHT);
        }
    }

    #[test]
    fn planar_policy_never_promotes_a_live_camera_to_its_baseline() {
        let (stage, _) = vector_stage();
        for resolution in [(854, 480), (64, 96)] {
            let original = camera(resolution);
            for change in 0..6 {
                let mut changed = original.clone();
                match change {
                    0 => {
                        changed.frame_mut().set_center([0.25, 0.0, 0.0]).unwrap();
                    }
                    1 => {
                        changed.frame_mut().set_height(4.0).unwrap();
                    }
                    2 => {
                        let width = changed.frame().width();
                        changed.frame_mut().set_width(width * 1.1).unwrap();
                    }
                    3 => {
                        changed
                            .frame_mut()
                            .set_orientation([0.0, 0.0, 0.6, 0.8])
                            .unwrap();
                    }
                    4 => {
                        changed.frame_mut().set_field_of_view(0.9).unwrap();
                    }
                    _ => {
                        changed
                            .set_pixel_shape(resolution.0 + 1, resolution.1)
                            .unwrap();
                    }
                }
                // Even constructing the policy after the mutation reads only
                // output parameters. It cannot bless an already-moved frame.
                let policy = PlanarCapturePolicy::for_output(resolution, FRAME_HEIGHT).unwrap();
                assert!(
                    policy.refusal(&stage, &changed).is_some(),
                    "change {change}"
                );
                assert_eq!(policy.refusal(&stage, &original), None);
            }
        }
    }

    #[test]
    fn planar_policy_preserves_height_and_geometry_refusals() {
        let (mut stage, mob) = vector_stage();
        let camera = camera((96, 64));
        let policy = PlanarCapturePolicy::for_output((96, 64), FRAME_HEIGHT).unwrap();
        for height in [4.0, 16.0, f64::NAN, f64::INFINITY] {
            let mismatch = PlanarCapturePolicy::for_output((96, 64), height).unwrap();
            assert_eq!(
                mismatch.refusal(&stage, &camera),
                Some("retained screen map differs from the output camera frame")
            );
        }
        for resolution in [(0, 64), (96, 0)] {
            assert_eq!(
                PlanarCapturePolicy::for_output(resolution, FRAME_HEIGHT),
                Err(CameraError::ZeroDimension)
            );
        }
        for change in 0..5 {
            let original = *stage.uniforms_mut(mob).unwrap();
            let uniforms = stage.uniforms_mut(mob).unwrap();
            match change {
                0 => uniforms.depth_test = true,
                1 => uniforms.shading = [0.2, 0.3, 0.4],
                2 => uniforms.clip_planes[0] = [1.0, 0.0, 0.0, 0.0],
                3 => uniforms.is_fixed_in_frame = 1.0,
                _ => uniforms.scale_stroke_with_zoom = true,
            }
            assert!(
                policy.refusal(&stage, &camera).is_some(),
                "uniform {change}"
            );
            *stage.uniforms_mut(mob).unwrap() = original;
            assert_eq!(policy.refusal(&stage, &camera), None);
        }
        stage.shift(mob, [0.0, 0.0, 0.5]);
        assert_eq!(
            policy.refusal(&stage, &camera),
            Some("nonplanar geometry requires a camera-bearing bundle")
        );
        stage.shift(mob, [0.0, 0.0, -0.5]);
        stage.apply_affine(
            mob,
            Placement::new(
                [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.5, 1.0]],
                [0.0; 3],
            ),
        );
        assert!(policy.refusal(&stage, &camera).is_some());
    }

    #[test]
    fn planar_policy_nonstandard_aspects_match_camera_pixels_and_worker_counts() {
        let stage = raster_fixture();
        // Keep the already-admitted 16:9 cases alongside the new aspects.
        // Camera samples=0 has native analytic AA; FastCpu is adaptive. A
        // direct comparison between them measures both sampling and projection
        // and already exceeds the FAST corpus RMS limit at 96x54. Compare
        // equal sampling under the existing FAST limits, then independently
        // check adaptive quality against both 4x references under the AA limits.
        for resolution in [
            (96, 54),
            (128, 72),
            (96, 64),
            (64, 96),
            (854, 480),
            (480, 854),
        ] {
            let camera = camera(resolution);
            let policy = PlanarCapturePolicy::for_output(resolution, FRAME_HEIGHT).unwrap();
            assert_eq!(policy.refusal(&stage, &camera), None);
            let config = RetainedFrameRendererConfig {
                frame: FrameConfig::new(
                    Viewport {
                        width: resolution.0,
                        height: resolution.1,
                    },
                    ScreenMap {
                        scale: f64::from(resolution.1) / FRAME_HEIGHT,
                        origin: [f64::from(resolution.0) / 2.0, f64::from(resolution.1) / 2.0],
                        y_up: true,
                    },
                    camera.background(),
                ),
                tiling: Tiling::default(),
                engine: EngineIdentity::fast(),
                threads: 1,
            };
            let mut camera_native = RetainedFrameRenderer::new(config).unwrap();
            camera_native.render_with_camera(&stage, &camera).unwrap();
            let mut camera_four = camera.clone();
            camera_four.set_samples(4);
            let mut projected_four = RetainedFrameRenderer::new(config).unwrap();
            projected_four
                .render_with_camera(&stage, &camera_four)
                .unwrap();
            let mut canonical = RetainedFrameRenderer::new(RetainedFrameRendererConfig {
                engine: EngineIdentity::certified(),
                ..config
            })
            .unwrap();
            canonical.render(&stage, 0).unwrap();
            let mut forced_four = RetainedFrameRenderer::new(RetainedFrameRendererConfig {
                frame: config.frame.with_aa_policy(fmn_core::AaPolicy::Ssaa4x),
                ..config
            })
            .unwrap();
            forced_four.render(&stage, 0).unwrap();
            for (name, a, b) in [
                (
                    "camera-native/certified-planar",
                    camera_native.frame(),
                    canonical.frame(),
                ),
                (
                    "camera-four/fast-four",
                    projected_four.frame(),
                    forced_four.frame(),
                ),
            ] {
                let (maximum, rms) = visual_error(a, b);
                eprintln!(
                    "{{\"case\":\"planar-output-projection\",\"width\":{},\"height\":{},\"pair\":\"{name}\",\"maximum\":{maximum:.17},\"rms\":{rms:.17}}}",
                    resolution.0, resolution.1,
                );
                assert!(
                    maximum <= FAST_VISUAL_BUDGET_V1_MAX_CHANNEL_ERROR,
                    "{resolution:?}, {name}: maximum {maximum}"
                );
                assert!(
                    rms <= FAST_VISUAL_BUDGET_V1_RMS_CHANNEL_ERROR,
                    "{resolution:?}, {name}: RMS {rms}"
                );
            }
            let mut first = Vec::new();
            for threads in [1, 4, 16] {
                let mut planar =
                    RetainedFrameRenderer::new(RetainedFrameRendererConfig { threads, ..config })
                        .unwrap();
                planar.render(&stage, 0).unwrap();
                if threads == 1 {
                    first = planar.frame().as_bytes().to_vec();
                    for (name, reference) in [
                        ("planar-four", forced_four.frame()),
                        ("camera-four", projected_four.frame()),
                    ] {
                        let (maximum, rms) = visual_error(planar.frame(), reference);
                        let (previous_maximum, previous_rms) =
                            visual_error(camera_native.frame(), reference);
                        // Both references are sampled, not exact full-pixel
                        // integrals. Keep the measured win/lose split visible;
                        // do not assert an accuracy ordering they cannot prove.
                        eprintln!(
                            "{{\"case\":\"planar-output-aa\",\"width\":{},\"height\":{},\"reference\":\"{name}\",\"maximum\":{maximum:.17},\"rms\":{rms:.17},\"previous_maximum\":{previous_maximum:.17},\"previous_rms\":{previous_rms:.17}}}",
                            resolution.0, resolution.1,
                        );
                        assert!(
                            maximum <= AA_VISUAL_BUDGET_V1_MAX_CHANNEL_ERROR,
                            "{resolution:?}, adaptive/{name}: maximum {maximum}"
                        );
                        assert!(
                            rms <= AA_VISUAL_BUDGET_V1_RMS_CHANNEL_ERROR,
                            "{resolution:?}, adaptive/{name}: RMS {rms}"
                        );
                    }
                } else {
                    assert_eq!(
                        planar.frame().as_bytes(),
                        first,
                        "{resolution:?}, {threads} workers"
                    );
                }
                planar.render(&stage, 0).unwrap();
                assert!(planar.last_frame_reused(), "static {resolution:?} raster");
                assert_eq!(planar.frame().as_bytes(), first);
            }
        }
    }
}
