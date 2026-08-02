//! W5 wasm tier 1 (fm-l97): the headless smoke probe for the wasm32
//! foundation. Compiled to a cdylib `.wasm` and instantiated directly by
//! `run.mjs` under node/bun — no wasm-bindgen CLI pass, so the probe proves
//! the *Rust* foundation executes in a real wasm VM rather than proving glue
//! generation.
//!
//! What the probes cover, end to end in the VM:
//!
//! - `render_probe_digest` / `render_probe_repeat_is_byte_identical` — the
//!   certified CPU render path (Stage → RenderPlan → MonoTable → Binning →
//!   FrameJob → `encode_frame` → digest) executing on wasm32, single-threaded
//!   by construction ([`fmn_render::effective_threads`] collapses any fan-out
//!   request), and byte-identical across two renders of the same primitive
//!   scene. This is the in-VM half of the bead's determinism contract; the
//!   host-side proxy lives in `crates/fmn-scene/tests/runtime.rs`.
//! - `clock_probe_monotonic_ms` / `clock_probe_wall_ms` —
//!   [`fmn_platform::clock::WasmClock`] reading `performance.now()` /
//!   `Date.now()` through its extern imports (the harness binds the real JS
//!   functions).
//! - `process_probe_capability_absent` —
//!   [`fmn_platform::process::NoProcessRunner`] failing closed with the named
//!   [`fmn_platform::process::ProcessError::CapabilityAbsent`] error: the
//!   ffmpeg boundary is structurally absent on wasm32.
//! - `topology_probe_single_threaded` —
//!   [`fmn_platform::topology::HardwareTopology::current`] reporting exactly
//!   one logical CPU (no atomics / cross-origin isolation is the documented
//!   tier-2 question, not this tier).
//!
//! The whole crate is wasm32-only: on any other target it is an empty
//! library, because every probe exists to exercise wasm-specific capability
//! implementations.
#![forbid(unsafe_code)]
#![cfg(target_arch = "wasm32")]

use std::time::Duration;

use wasm_bindgen::prelude::wasm_bindgen;

use fmn_core::color::Srgb;
use fmn_mobject::{Mobject, Stage};
use fmn_platform::clock::{Clock, WasmClock};
use fmn_platform::process::{NoProcessRunner, ProcessError, ProcessRunner, ProcessSpec};
use fmn_platform::topology::HardwareTopology;
use fmn_render::bin::{Binning, ScreenMap, Tiling, Viewport};
use fmn_render::engine::{FrameConfig, FrameJob, encode_frame, frame_digest};
use fmn_render::fill::MonoTable;
use fmn_render::plan::RenderPlan;

/// The probe scene is deliberately tiny: the smoke test measures execution
/// and determinism, not coverage (the golden corpora own coverage).
const WIDTH: u32 = 96;
const HEIGHT: u32 = 54;
const TILING: Tiling = Tiling {
    macro_tile: 64,
    fine_tile: 8,
};

fn frame_config() -> FrameConfig {
    FrameConfig::new(
        Viewport {
            width: WIDTH,
            height: HEIGHT,
        },
        ScreenMap {
            scale: 20.0,
            origin: [f64::from(WIDTH) / 2.0, f64::from(HEIGHT) / 2.0],
        },
        Srgb::from_rgb8(0x22, 0x22, 0x22).to_linear(1.0),
    )
}

/// One small primitive scene: a filled square, shifted off-center so the
/// rasterizer's coverage path is genuinely exercised. The point run is the
/// quad-path anchor/handle interleave a library builder would emit
/// (`set_points_as_corners` over the closed corner ring), written over the
/// VMobject record schema with an opaque fill — a bare
/// `Mobject::from_points` carries neither path structure nor style and
/// would rasterize to background, making the probe vacuous.
fn probe_stage() -> Stage {
    let points: [[f32; 3]; 9] = [
        [-1.0, -1.0, 0.0],
        [0.0, -1.0, 0.0],
        [1.0, -1.0, 0.0],
        [1.0, 0.0, 0.0],
        [1.0, 1.0, 0.0],
        [0.0, 1.0, 0.0],
        [-1.0, 1.0, 0.0],
        [-1.0, 0.0, 0.0],
        [-1.0, -1.0, 0.0],
    ];
    let mut buffer =
        fmn_mobject::RecordBuffer::new(fmn_mobject::RecordSchema::vmobject(), points.len())
            .expect("nine probe records cannot overflow the buffer size");
    for (i, point) in points.iter().enumerate() {
        buffer.write(i, "point", point);
        buffer.write(i, "fill_rgba", &[1.0, 0.0, 0.0, 1.0]);
        buffer.write(i, "stroke_rgba", &[1.0, 1.0, 1.0, 1.0]);
        buffer.write(i, "stroke_width", &[4.0]);
    }
    let mut stage = Stage::new();
    let mob = stage.add(Mobject::from_buffer(buffer));
    stage.add_to_scene(mob).expect("live root");
    stage.shift(mob, [0.5, 0.25, 0.0]);
    stage
}

/// Render the probe scene single-threaded — the exact call shape the wasm
/// tier-1 surface uses — and return the canonical encoded frame bytes.
fn render_probe_frame() -> Vec<u8> {
    let stage = probe_stage();
    let config = frame_config();
    let mut plan = RenderPlan::new();
    plan.sync(&stage, 0).expect("valid wasm smoke fixture");
    let mono = MonoTable::build(&plan, config.map);
    let mut binning = Binning::build(&plan, config.viewport, TILING, config.map)
        .expect("bounded wasm smoke binning");
    binning.prune_occluded(&plan).expect("binning prune");
    let job = FrameJob::new(&plan, &mono, &binning, config).expect("frame job");
    // threads = 1: the wasm32 configuration. (`effective_threads` would
    // collapse a larger request to the same serial path on this target.)
    let frame = job.render(1).expect("render");
    encode_frame(&frame).expect("encode")
}

/// The frame digest of the probe scene, truncated to 64 bits for the JS
/// boundary (the full digest is compared in-process by
/// [`render_probe_repeat_is_byte_identical`]).
#[wasm_bindgen]
pub fn render_probe_digest() -> u64 {
    let stage = probe_stage();
    let config = frame_config();
    let mut plan = RenderPlan::new();
    plan.sync(&stage, 0).expect("valid wasm smoke fixture");
    let mono = MonoTable::build(&plan, config.map);
    let mut binning = Binning::build(&plan, config.viewport, TILING, config.map)
        .expect("bounded wasm smoke binning");
    binning.prune_occluded(&plan).expect("binning prune");
    let job = FrameJob::new(&plan, &mono, &binning, config).expect("frame job");
    let frame = job.render(1).expect("render");
    let digest = frame_digest(&frame).expect("digest");
    u64::from_be_bytes(digest.as_bytes()[..8].try_into().expect("digest prefix"))
}

/// The determinism contract, proven inside the wasm VM: two renders of the
/// same scene under the single-thread configuration produce byte-identical
/// canonical frames.
#[wasm_bindgen]
pub fn render_probe_repeat_is_byte_identical() -> bool {
    render_probe_frame() == render_probe_frame()
}

/// The vacuity guard: the probe scene must actually draw something. Renders
/// the probe and an empty stage and reports whether the canonical bytes
/// differ — without it, byte-identical repeat renders could be two identical
/// background-only frames and the determinism proof would say nothing.
#[wasm_bindgen]
pub fn render_probe_is_not_background() -> bool {
    let probe = render_probe_frame();
    let background = {
        let stage = Stage::new();
        let config = frame_config();
        let mut plan = RenderPlan::new();
        plan.sync(&stage, 0)
            .expect("valid empty wasm smoke fixture");
        let mono = MonoTable::build(&plan, config.map);
        let mut binning = Binning::build(&plan, config.viewport, TILING, config.map)
            .expect("bounded wasm smoke binning");
        binning.prune_occluded(&plan).expect("binning prune");
        let job = FrameJob::new(&plan, &mono, &binning, config).expect("frame job");
        let frame = job.render(1).expect("render");
        encode_frame(&frame).expect("encode")
    };
    probe != background
}

/// Monotonic milliseconds from the browser clock capability.
#[wasm_bindgen]
pub fn clock_probe_monotonic_ms() -> f64 {
    WasmClock::new().monotonic().as_secs_f64() * 1000.0
}

/// Wall-clock milliseconds since the Unix epoch from the browser clock
/// capability.
#[wasm_bindgen]
pub fn clock_probe_wall_ms() -> f64 {
    let wall = WasmClock::new().wall();
    wall.duration_since(std::time::SystemTime::UNIX_EPOCH)
        .unwrap_or(Duration::ZERO)
        .as_secs_f64()
        * 1000.0
}

/// The process capability fails closed on wasm32: every request is the named
/// [`ProcessError::CapabilityAbsent`], never a spawn attempt.
#[wasm_bindgen]
pub fn process_probe_capability_absent() -> bool {
    let spec = ProcessSpec {
        program: "/nonexistent/ffmpeg".into(),
        argv: Vec::new(),
        env: Vec::new(),
        cwd: None,
        stdin: None,
        timeout: Duration::from_secs(1),
        max_output_bytes: 1024,
    };
    matches!(
        NoProcessRunner.run(&spec),
        Err(ProcessError::CapabilityAbsent { .. })
    )
}

/// The planner-visible machine shape on wasm32 is exactly one logical CPU.
#[wasm_bindgen]
pub fn topology_probe_single_threaded() -> bool {
    let topology = HardwareTopology::current();
    topology.logical_cores() == 1 && topology.physical_cores == 1 && !topology.smt_active()
}
// fingerprint probe
