//! Live pure programs use bounded render teams and the ordinary output owner.

use fmn::rendering::NativeFramePipeline;
use fmn_core::color::LinearRgba;
use fmn_frame::convert::{rgba_to_nv12, rgba16f_to_p010, rgba16f_to_rgba8, swap_rb8};
use fmn_frame::{ChromaSiting, ColorRange, FrameBuffer, FrameLayout, PixelFormat};
use fmn_mobject::{Mobject, RecordBuffer, RecordSchema, Stage};
use fmn_output::{EmitterConfig, OrderedEmitter, SinkBinding, SinkWrite};
use fmn_platform::topology::HardwareTopology;
use fmn_render::{
    EngineIdentity, FrameConfig, RetainedFrameRenderer, RetainedFrameRendererConfig, ScreenMap,
    Tiling, Viewport,
};
use fmn_runtime::{ExecutionPlan, OutputPixelFormat, PlanRequest, RenderIntent, SurfaceSpec};
use std::sync::{Arc, Mutex};

fn plan(format: OutputPixelFormat) -> ExecutionPlan {
    let mut surface = SurfaceSpec::lumen(32, 24);
    if format != OutputPixelFormat::Rgba8 {
        surface.working_bytes_per_pixel += 4;
    }
    // Two small processor groups guarantee independent teams even on a
    // single-core CI host, without spawning workstation-sized worker pools.
    let topology = HardwareTopology::from_group_sizes(&[2, 2]).unwrap();
    ExecutionPlan::derive(
        PlanRequest::certified(RenderIntent::Offline, surface, format).with_max_frames_in_flight(3),
        &topology,
        None,
    )
    .unwrap()
}

fn config(plan: &ExecutionPlan) -> RetainedFrameRendererConfig {
    RetainedFrameRendererConfig {
        frame: FrameConfig::new(
            Viewport {
                width: 32,
                height: 24,
            },
            ScreenMap {
                scale: 4.0,
                origin: [16.0, 12.0],
                y_up: true,
            },
            LinearRgba {
                r: 0.0,
                g: 0.0,
                b: 0.0,
                a: 1.0,
            },
        ),
        tiling: Tiling {
            macro_tile: plan.macro_tile,
            fine_tile: plan.fine_tile,
        },
        engine: EngineIdentity::certified(),
        threads: 1,
    }
}

fn shape() -> Mobject {
    let mut records = RecordBuffer::new(RecordSchema::vmobject(), 3).unwrap();
    records.write_range(
        "point",
        0,
        &[-1.5, -1.0, 0.0, 0.0, 1.25, 0.0, 1.5, -1.0, 0.0],
    );
    records.write_range("fill_rgba", 0, &[0.2, 0.6, 1.0, 1.0].repeat(3));
    Mobject::from_buffer(records)
}

/// The serial two-step route for 8-bit wires (the pipeline's fused kernels
/// must reproduce it byte for byte), and the direct binary16 kernel for P010,
/// whose precision beyond eight bits the two-step route would discard.
fn convert_reference(frame: &FrameBuffer, format: PixelFormat) -> Vec<u8> {
    if format == PixelFormat::P010 {
        let mut output = FrameBuffer::new(FrameLayout::tight(format, 32, 24).unwrap());
        rgba16f_to_p010(frame, &mut output, ColorRange::Limited, ChromaSiting::Left).unwrap();
        return output.as_bytes().to_vec();
    }
    let mut rgba = FrameBuffer::new(FrameLayout::tight(PixelFormat::Rgba8, 32, 24).unwrap());
    rgba16f_to_rgba8(frame, &mut rgba).unwrap();
    if format == PixelFormat::Rgba8 {
        return rgba.as_bytes().to_vec();
    }
    let mut output = FrameBuffer::new(FrameLayout::tight(format, 32, 24).unwrap());
    match format {
        PixelFormat::Bgra8 => swap_rb8(&rgba, &mut output),
        PixelFormat::Nv12 => {
            rgba_to_nv12(&rgba, &mut output, ColorRange::Limited, ChromaSiting::Left)
        }
        _ => panic!("not a converted CPU format"),
    }
    .unwrap();
    output.as_bytes().to_vec()
}

use fmn_anim::{AnimConfig, Animation, RateFunc, RationalFrameClock, Transform, open_play, advance_play};
use fmn_anim::pure_frame::PureSegment;
use fmn_core::rng::RngRoot;

#[test]
fn live_programs_match_serial_lumen_on_every_wire_and_team() {
    for (negotiated, format) in [
        (OutputPixelFormat::Rgba8, PixelFormat::Rgba8),
        (OutputPixelFormat::Bgra8, PixelFormat::Bgra8),
        (OutputPixelFormat::Nv12, PixelFormat::Nv12),
        (OutputPixelFormat::P010, PixelFormat::P010),
    ] {
        let plan = plan(negotiated);
        assert_eq!(plan.render_teams.len(), 2);
        let config = config(&plan);
        let received = Arc::new(Mutex::new(Vec::new()));
        let output = received.clone();
        let emitter = OrderedEmitter::new(
            EmitterConfig::new(FrameLayout::tight(format, 32, 24).unwrap(), plan.frames_in_flight, 0).unwrap(),
            vec![SinkBinding::reliable("record", move |sequence, frame: &FrameBuffer| {
                output.lock().unwrap().push((sequence, frame.as_bytes().to_vec()));
                Ok(SinkWrite::Consumed)
            })],
        ).unwrap();
        let mut pipeline = NativeFramePipeline::new(plan.clone(), config, None, emitter.handle()).unwrap();
        let mut stage = Stage::new();
        let mob = stage.add(shape());
        stage.add_to_scene(mob).unwrap();
        let target = stage.copy_family(mob).unwrap();
        stage.shift(target, [2.0, 0.25, 0.0]);
        let mut animations: Vec<Box<dyn Animation>> = vec![Box::new(Transform::new(mob, target)
            .with_path_arc(0.8, [0.0, 0.0, 1.0]).with_config(AnimConfig {
                run_time: 1.5, rate_func: RateFunc::linear(), ..AnimConfig::default()
            }))];
        let rng = RngRoot::from_seed(92);
        let mut clock = RationalFrameClock::new(8).unwrap();
        clock.advance_frames(6).unwrap();
        stage.set_time_from_clock(clock.now().to_f64());
        let mut open = open_play(&mut stage, &clock, &mut animations).unwrap();
        let program = PureSegment::freeze(&stage, &clock, &rng, &animations, &open).unwrap();
        let mut serial = RetainedFrameRenderer::new(config).unwrap();
        let mut expected = Vec::new();
        advance_play(&mut stage, &mut clock, &rng, &mut animations, &mut open,
            program.frame_count(), &mut |packet| {
                serial.render(&packet.materialize_stage(), 0).unwrap();
                expected.push((expected.len() as u64, convert_reference(serial.frame(), format)));
            }).unwrap();
        // An earlier ordinary capture can coexist with a pure generation;
        // leave it off this comparison so no extra sample is inserted.
        drop(stage);
        drop(animations);
        for index in 0..program.frame_count() {
            let job = program.job(index + 1).unwrap();
            assert_eq!(job.time().frames(), index + 7);
            pipeline.capture_pure(job, index as u64).unwrap();
            if index % 4 == 3 {
                let barrier = pipeline.flush().unwrap();
                assert_eq!(barrier.emitted, index as u64 + 1);
                assert_eq!(barrier.outstanding_slots, 0);
            }
        }
        drop(program);
        let stats = pipeline.finish().unwrap();
        let output = emitter.finish().unwrap();
        assert_eq!(stats.emitted, 12);
        assert!(stats.render_team_frames.iter().all(|count| *count > 0));
        assert!(stats.max_in_flight <= plan.frames_in_flight);
        assert_eq!(stats.outstanding_slots, 0);
        assert_eq!(output.stats.outstanding, 0);
        assert_eq!(*received.lock().unwrap(), expected);
        assert_ne!(expected[0].1, expected[11].1);
    }
}

#[test]
fn pure_job_cancellation_releases_all_output_reservations() {
    let plan = plan(OutputPixelFormat::Rgba8);
    let config = config(&plan);
    let emitter = OrderedEmitter::new(
        EmitterConfig::new(FrameLayout::tight(PixelFormat::Rgba8, 32, 24).unwrap(), plan.frames_in_flight, 0).unwrap(),
        vec![SinkBinding::reliable("record", |_, _: &FrameBuffer| Ok(SinkWrite::Consumed))],
    ).unwrap();
    let handle = emitter.handle();
    let mut pipeline = NativeFramePipeline::new(plan, config, None, handle.clone()).unwrap();
    let mut stage = Stage::new();
    let mob = stage.add(shape());
    stage.add_to_scene(mob).unwrap();
    let target = stage.copy_family(mob).unwrap();
    stage.shift(target, [1.0, 0.0, 0.0]);
    let mut animations: Vec<Box<dyn Animation>> = vec![Box::new(Transform::new(mob, target))];
    let clock = RationalFrameClock::new(8).unwrap();
    let open = open_play(&mut stage, &clock, &mut animations).unwrap();
    let program = PureSegment::freeze(&stage, &clock, &RngRoot::from_seed(3), &animations, &open).unwrap();
    pipeline.capture_pure(program.job(1).unwrap(), 0).unwrap();
    emitter.cancel();
    let _ = pipeline.capture_pure(program.job(2).unwrap(), 1);
    assert!(pipeline.finish().is_err());
    let output = emitter.finish();
    assert!(output.is_err());
    assert_eq!(handle.stats().outstanding, 0);
}
