use std::sync::{Arc, Mutex};

use fmn_anim::pure_frame::PureSegment;
use fmn_anim::{
    AnimConfig, Animation, RateFunc, RationalFrameClock, Transform, advance_play, open_play,
};
use fmn_core::{color::LinearRgba, rate::CatalogRate, rng::RngRoot};
use fmn_frame::{FrameBuffer, FrameLayout, PixelFormat};
use fmn_mobject::{Mobject, RecordBuffer, RecordSchema, Stage};
use fmn_output::{EmitterConfig, OrderedEmitter, SinkBinding, SinkWrite};
use fmn_platform::topology::HardwareTopology;
use fmn_render::{
    Camera, CameraConfig, EngineIdentity, FrameConfig, RetainedFrameRenderer,
    RetainedFrameRendererConfig, ScreenMap, Tiling, Viewport,
};
use fmn_runtime::{ExecutionPlan, OutputPixelFormat, PlanRequest, RenderIntent, SurfaceSpec};

use super::NativeAdaptiveCamera;
use crate::rendering::pipeline::{NativeFramePipeline, convert_into};

fn plan(certified: bool, threads: usize, wire: OutputPixelFormat) -> ExecutionPlan {
    let mut surface = SurfaceSpec::lumen(32, 18);
    surface.working_bytes_per_pixel = 36;
    let request = if certified {
        PlanRequest::certified(RenderIntent::Offline, surface, wire)
    } else {
        PlanRequest::standard(RenderIntent::Offline, surface, wire)
    };
    ExecutionPlan::derive(
        request
            .with_max_cpu_threads(threads)
            .with_max_frames_in_flight(4),
        &HardwareTopology::from_group_sizes(&[8, 8]).unwrap(),
        None,
    )
    .unwrap()
}

fn fixture() -> (Stage, Vec<Box<dyn Animation>>) {
    let mut records = RecordBuffer::new(RecordSchema::vmobject(), 3).unwrap();
    records.write_range(
        "point",
        0,
        &[-2.0, -1.5, 0.0, 0.0, 1.5, 0.0, 2.0, -1.5, 0.0],
    );
    records.write_range("fill_rgba", 0, &[0.2, 0.6, 1.0, 0.7].repeat(3));
    let mut stage = Stage::new();
    let mob = stage.add(Mobject::from_buffer(records));
    stage.add_to_scene(mob).unwrap();
    let target = stage.copy_family(mob).unwrap();
    stage.shift(target, [0.75, 0.0, 0.5]);
    let animation = Transform::new(mob, target).with_config(AnimConfig {
        run_time: 0.41,
        rate_func: RateFunc::Catalog(CatalogRate::ThereAndBack).squish(0.25, 0.75),
        ..AnimConfig::default()
    });
    (stage, vec![Box::new(animation)])
}

fn camera() -> Camera {
    Camera::new(CameraConfig {
        resolution: (32, 18),
        fps: 10,
        background: LinearRgba {
            r: 0.07,
            g: 0.03,
            b: 0.1,
            a: 1.0,
        },
        ..CameraConfig::default()
    })
    .unwrap()
}

fn renderer_config(plan: &ExecutionPlan, camera: &Camera) -> RetainedFrameRendererConfig {
    RetainedFrameRendererConfig {
        frame: FrameConfig::new(
            Viewport {
                width: 32,
                height: 18,
            },
            ScreenMap {
                scale: 18.0 / 8.0,
                origin: [16.0, 9.0],
                y_up: true,
            },
            camera.background(),
        ),
        tiling: Tiling {
            macro_tile: plan.macro_tile,
            fine_tile: plan.fine_tile,
        },
        engine: match plan.engine {
            fmn_runtime::ExecutionEngine::CertifiedCpu => EngineIdentity::certified(),
            _ => EngineIdentity::fast(),
        },
        threads: 1,
    }
}

#[test]
fn pure_worker_routes_match_serial_pixels_when_a_transform_enters_and_leaves_3d() {
    for certified in [true, false] {
        for threads in [1, 4, 16] {
            for (wire, pixel) in [
                (OutputPixelFormat::Rgba8, PixelFormat::Rgba8),
                (OutputPixelFormat::Nv12, PixelFormat::Nv12),
            ] {
                let plan = plan(certified, threads, wire);
                let teams = plan.render_teams.len();
                let camera = camera();
                let config = renderer_config(&plan, &camera);
                let layout = FrameLayout::tight(pixel, 32, 18).unwrap();
                let (mut stage, mut animations) = fixture();
                let rng = RngRoot::from_seed(19);
                let mut clock = RationalFrameClock::new(10).unwrap();
                clock.advance_frames(7).unwrap();
                stage.set_time_from_clock(clock.now().to_f64());
                let mut open = open_play(&mut stage, &clock, &mut animations).unwrap();
                let segment =
                    PureSegment::freeze(&stage, &clock, &rng, &animations, &open).unwrap();
                assert_eq!(
                    segment.frame_count(),
                    5,
                    "fractional duration rounds up on Choreo's grid"
                );
                let mut serial = RetainedFrameRenderer::new(config).unwrap();
                let mut expected = Vec::new();
                advance_play(
                    &mut stage,
                    &mut clock,
                    &rng,
                    &mut animations,
                    &mut open,
                    5,
                    &mut |packet| {
                        let state = packet.materialize_stage();
                        // Independent route oracle for this known out-and-back
                        // motion: samples 2 and 3 alone have nonzero world z.
                        if matches!(packet.segment_frame(), 2 | 3) {
                            serial.render_with_camera(&state, &camera).unwrap();
                        } else {
                            serial.render(&state, 0).unwrap();
                        }
                        let mut frame = FrameBuffer::new(layout.clone());
                        convert_into(serial.frame(), &mut frame, 1).unwrap();
                        expected.push(frame.as_bytes().to_vec());
                    },
                )
                .unwrap();
                let received = Arc::new(Mutex::new(Vec::new()));
                let target = Arc::clone(&received);
                let emitter = OrderedEmitter::new(
                    EmitterConfig::new(layout, plan.frames_in_flight, 20).unwrap(),
                    vec![SinkBinding::reliable(
                        "observed",
                        move |sequence, frame: &FrameBuffer| {
                            target
                                .lock()
                                .unwrap()
                                .push((sequence, frame.as_bytes().to_vec()));
                            Ok(SinkWrite::Consumed)
                        },
                    )],
                )
                .unwrap();
                let mut pipeline =
                    NativeFramePipeline::new(plan, config, None, emitter.handle()).unwrap();
                let policy = NativeAdaptiveCamera::new(camera.clone(), true, None);
                for frame in 1..=5 {
                    pipeline
                        .capture_pure_adaptive(
                            segment.job(frame).unwrap(),
                            19 + frame as u64,
                            policy.clone(),
                        )
                        .unwrap();
                }
                let stats = pipeline.finish().unwrap();
                emitter.finish().unwrap();
                assert_eq!(
                    (stats.submitted, stats.emitted, stats.outstanding_slots),
                    (5, 5, 0)
                );
                assert_eq!(stats.render_team_frames.len(), teams);
                assert!(stats.render_team_frames.iter().all(|frames| *frames > 0));
                let received = received.lock().unwrap();
                assert_eq!(received.len(), expected.len());
                for (index, ((sequence, actual), expected)) in
                    received.iter().zip(&expected).enumerate()
                {
                    assert_eq!(*sequence, 20 + index as u64);
                    assert_eq!(
                        actual, expected,
                        "mode={certified}, threads={threads}, frame={index}"
                    );
                }
                let routes = policy.routes().unwrap();
                assert_eq!((routes.planar, routes.camera), (3, 2));
                assert!(routes.first_camera_reason.unwrap().contains("nonplanar"));
            }
        }
    }
}

#[test]
fn native_light_is_read_from_the_sample_and_stale_lights_refuse() {
    let (mut stage, _) = fixture();
    let light = stage.add(Mobject::from_points(&[[1.0, 2.0, 3.0]]));
    let policy = NativeAdaptiveCamera::new(camera(), false, Some(light));
    let (capture, _) = policy.for_stage(&stage).unwrap();
    assert_eq!(capture.unwrap().light_source_position(), [1.0, 2.0, 3.0]);
    stage.shift(light, [2.0, 3.0, 4.0]);
    let (capture, _) = policy.for_stage(&stage).unwrap();
    assert_eq!(capture.unwrap().light_source_position(), [3.0, 5.0, 7.0]);
    stage.delete(light).unwrap();
    assert!(
        policy
            .for_stage(&stage)
            .unwrap_err()
            .to_string()
            .contains("light handle is stale")
    );
}

#[test]
fn route_diagnostics_follow_output_order_despite_worker_completion_order() {
    let policy = NativeAdaptiveCamera::new(camera(), true, None);
    policy.record(12, Some("later")).unwrap();
    policy.record(10, Some("earlier")).unwrap();
    policy.record(11, None).unwrap();
    let routes = policy.routes().unwrap();
    assert_eq!((routes.planar, routes.camera), (1, 2));
    assert_eq!(routes.first_camera_reason, Some("earlier"));
}
