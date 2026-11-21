//! Real standard-mode HDR frames through native workers and ordered output.

use std::cell::Cell;
use std::path::Path;
use std::rc::Rc;
use std::sync::{Arc, Mutex};

use fmn::rendering::{NativeFramePipeline, RenderError, YuvConversionPolicy};
use fmn_conformance::e2e::{LogEvent, RunCtx, RunOutcome, ScenarioError};
use fmn_core::color::LinearRgba;
use fmn_frame::convert::rgba16f_to_p010_hdr_pq;
use fmn_frame::{
    ChromaSiting, ColorRange, FrameBuffer, FrameLayout, HdrPqConfig, PixelFormat, VideoColorGrade,
};
use fmn_mobject::{Mobject, RecordBuffer, RecordSchema, Stage};
use fmn_output::{EmitterConfig, OrderedEmitter, SinkBinding, SinkWrite};
use fmn_platform::fs::{FileSystem, VirtualFs};
use fmn_platform::topology::HardwareTopology;
use fmn_render::{
    EngineIdentity, FrameConfig, RetainedFrameRenderer, RetainedFrameRendererConfig, ScreenMap,
    Tiling, Viewport,
};
use fmn_runtime::{ExecutionPlan, OutputPixelFormat, PlanRequest, RenderIntent, SurfaceSpec};

const WIDTH: u32 = 64;
const HEIGHT: u32 = 256;
const FRAMES: u64 = 8;

fn failure(error: impl std::fmt::Display) -> ScenarioError {
    super::fail(format!("HDR PQ scenario: {error}"))
}

fn bright_shape() -> Result<Mobject, ScenarioError> {
    let mut records = RecordBuffer::new(RecordSchema::vmobject(), 3).map_err(failure)?;
    if !records.write_range(
        "point",
        0,
        &[-1.5, -1.0, 0.0, 0.0, 1.25, 0.0, 1.5, -1.0, 0.0],
    ) || !records.write_range("fill_rgba", 0, &[2.0, 2.0, 2.0, 1.0].repeat(3))
    {
        return Err(failure("could not author highlight fixture"));
    }
    Ok(Mobject::from_buffer(records))
}

fn worker_frames(threads: u32) -> Result<Vec<(u64, Vec<u8>)>, ScenarioError> {
    let topology = HardwareTopology::from_group_sizes(&[threads]).map_err(failure)?;
    let plan = ExecutionPlan::derive(
        PlanRequest::standard(
            RenderIntent::Offline,
            SurfaceSpec::lumen(WIDTH, HEIGHT),
            OutputPixelFormat::P010,
        )
        .with_max_frames_in_flight(2),
        &topology,
        None,
    )
    .map_err(failure)?;
    if plan.render_teams.len() != 1 || plan.render_teams[0].threads() != threads as usize {
        return Err(failure(
            "topology did not admit the requested HDR conversion workers",
        ));
    }
    let config = RetainedFrameRendererConfig {
        frame: FrameConfig::new(
            Viewport {
                width: WIDTH,
                height: HEIGHT,
            },
            ScreenMap {
                scale: 16.0,
                origin: [32.0, 128.0],
                y_up: true,
            },
            LinearRgba {
                r: 1.0,
                g: 1.0,
                b: 1.0,
                a: 1.0,
            },
        ),
        tiling: Tiling {
            macro_tile: plan.macro_tile,
            fine_tile: plan.fine_tile,
        },
        engine: EngineIdentity::fast(),
        threads: 1,
    };
    let layout = FrameLayout::tight(PixelFormat::P010, WIDTH, HEIGHT).map_err(failure)?;
    let received = Arc::new(Mutex::new(Vec::new()));
    let output = received.clone();
    let emitter = OrderedEmitter::new(
        EmitterConfig::new(layout.clone(), plan.frames_in_flight, 0).map_err(failure)?,
        vec![SinkBinding::reliable(
            "hdr-pq-wire",
            move |sequence, frame: &FrameBuffer| {
                output
                    .lock()
                    .unwrap()
                    .push((sequence, frame.as_bytes().to_vec()));
                Ok(SinkWrite::Consumed)
            },
        )],
    )
    .map_err(failure)?;
    let mut transparent = config;
    transparent.frame.background.a = 0.5;
    match NativeFramePipeline::with_output_color_policy(
        plan.clone(),
        transparent,
        None,
        emitter.handle(),
        0,
        0,
        0,
        VideoColorGrade::default(),
        YuvConversionPolicy::Scoped,
        Some(HdrPqConfig::default()),
    ) {
        Err(RenderError::InvalidOptions(message)) if message.contains("opaque") => {}
        Err(error) => {
            return Err(failure(format!(
                "unexpected transparent pipeline refusal: {error}"
            )));
        }
        Ok(_) => return Err(failure("HDR pipeline accepted uncomposited transparency")),
    }
    if emitter.stats().published != 0 || emitter.stats().outstanding != 0 {
        return Err(failure("transparent HDR admission consumed an output slot"));
    }
    let mut pipeline = NativeFramePipeline::with_output_color_policy(
        plan.clone(),
        config,
        None,
        emitter.handle(),
        0,
        0,
        0,
        VideoColorGrade::default(),
        YuvConversionPolicy::Scoped,
        Some(HdrPqConfig::default()),
    )
    .map_err(failure)?;
    let mut serial = RetainedFrameRenderer::new(config).map_err(failure)?;
    let mut stage = Stage::new();
    let shape = stage.add(bright_shape()?);
    stage.add_to_scene(shape).map_err(failure)?;
    let mut expected = Vec::new();
    for sequence in 0..FRAMES {
        if sequence == 4 {
            stage.shift(shape, [0.5, 0.0, 0.0]);
        }
        serial.render(&stage, 0).map_err(failure)?;
        let mut frame = FrameBuffer::new(layout.clone());
        rgba16f_to_p010_hdr_pq(
            serial.frame(),
            &mut frame,
            ColorRange::Limited,
            ChromaSiting::Left,
            HdrPqConfig::default(),
        )
        .map_err(failure)?;
        expected.push((sequence, frame.as_bytes().to_vec()));
        pipeline.capture(&stage, sequence).map_err(failure)?;
        if sequence == 3 {
            let barrier = pipeline.flush().map_err(failure)?;
            if barrier.emitted != 4 || barrier.outstanding_slots != 0 {
                return Err(failure("HDR barrier lost frames or retained a frame slot"));
            }
        }
    }
    drop(stage);
    let stats = pipeline.finish().map_err(failure)?;
    let report = emitter.finish().map_err(failure)?;
    if (stats.submitted, stats.converted, stats.emitted) != (FRAMES, FRAMES, FRAMES)
        || stats.outstanding_slots != 0
        || stats.max_in_flight > plan.frames_in_flight
        || report.stats.outstanding != 0
        || report.stats.emitted != FRAMES
    {
        return Err(failure(
            "HDR worker publication violated bounded ordered output",
        ));
    }
    let actual = received.lock().unwrap().clone();
    if actual != expected {
        return Err(failure(
            "worker HDR bytes differ from serial live Lumen conversion",
        ));
    }
    if actual[0].1 != actual[3].1 || actual[4].1 != actual[7].1 || actual[0].1 == actual[4].1 {
        return Err(failure(
            "repeated or moved HDR captures lost their exact frame identity",
        ));
    }
    let first_luma = &actual[0].1[..WIDTH as usize * HEIGHT as usize * 2];
    let first_code = u16::from_le_bytes([first_luma[0], first_luma[1]]) >> 6;
    let highlights = first_luma
        .chunks_exact(2)
        .any(|bytes| u16::from_le_bytes([bytes[0], bytes[1]]) >> 6 > 573);
    if first_code != 573 || !highlights {
        return Err(failure(
            "HDR output lost 203-nit reference white or clipped all highlight headroom",
        ));
    }
    Ok(actual)
}

fn early_refusals() -> Result<u64, ScenarioError> {
    struct CountConstruct(Rc<Cell<u64>>);
    impl fmn::SceneConstruct for CountConstruct {
        fn construct(&mut self, _: &mut fmn::Stage<'_>) -> fmn::Result<()> {
            self.0.set(self.0.get() + 1);
            Ok(())
        }
    }
    let calls = Rc::new(Cell::new(0));
    let fs = Arc::new(VirtualFs::new());
    for case in 0..5 {
        let path = format!("/refused-hdr-{case}.mp4");
        let format = if case == 0 {
            fmn::RenderFormat::Png
        } else {
            fmn::RenderFormat::Mp4
        };
        let mut options =
            fmn::RenderOptions::with_format(path.as_str(), format).map_err(failure)?;
        options.config.camera.resolution = (WIDTH, HEIGHT);
        options.config.determinism.mode = fmn_config::config::DeterminismMode::Standard;
        options.typeset_cache = false;
        options.hdr_pq = Some(HdrPqConfig::default());
        let expected = match case {
            0 => "hdr_pq requires opaque MP4/MOV",
            1 => {
                options.config.file_writer.gamma = 2.0;
                "hdr_pq requires identity movie gamma and saturation"
            }
            2 => {
                options.yuv_conversion = YuvConversionPolicy::Persistent;
                "hdr_pq requires scoped YUV conversion"
            }
            3 => {
                options.config.camera.background_opacity = 0.5;
                "hdr_pq requires opaque MP4/MOV"
            }
            _ => {
                options.config.determinism.mode = fmn_config::config::DeterminismMode::Certified;
                "outside the certified artifact set"
            }
        };
        match fmn::render_with_fs(&mut CountConstruct(calls.clone()), options, fs.clone()) {
            Err(error) if error.to_string().contains(expected) => {}
            Err(error) => return Err(failure(format!("unexpected refusal: {error}"))),
            Ok(_) => return Err(failure("unsupported HDR request was accepted")),
        }
        if calls.get() != 0 || fs.exists(Path::new(&path)) {
            return Err(failure(
                "HDR refusal executed scene code or published output",
            ));
        }
    }
    Ok(5)
}

pub(super) fn run(ctx: &mut RunCtx) -> Result<RunOutcome, ScenarioError> {
    ctx.set_fps((8, 1));
    let serial = worker_frames(1)?;
    let parallel = worker_frames(4)?;
    if serial != parallel {
        return Err(failure(
            "HDR output changed with render/conversion worker count",
        ));
    }
    let refusals = early_refusals()?;
    ctx.event(
        LogEvent::new("e2e.parity.native_hdr_pq")
            .field("mode", "standard")
            .field("wire", "bt2020-pq-p010-limited")
            .field("thread_configs", 2_u64)
            .field("frames", FRAMES * 2)
            .field("exact_bytes", "true")
            .field("highlight_headroom", "true")
            .field("direct_pipeline_refusals", 2_u64)
            .field("early_refusals", refusals)
            .field("refused_constructors", 0_u64),
    );
    Ok(RunOutcome::ok()
        .with_counter("hdr_pq_thread_configs", 2)
        .with_counter("hdr_pq_frames", FRAMES * 2)
        .with_counter("hdr_pq_direct_pipeline_refusals", 2)
        .with_counter("hdr_pq_early_refusals", refusals)
        .with_counter("hdr_pq_refused_constructors", 0)
        .with_counter("hdr_pq_outstanding_slots", 0)
        .with_artifact("hdr-pq-frame.p010", serial[0].1.clone()))
}
