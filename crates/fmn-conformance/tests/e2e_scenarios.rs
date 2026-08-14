//! fm-fjq seed scenario catalog: the Gauntlet's e2e scenarios **as data**
//! over the [`fmn_conformance::e2e`] harness API.
//!
//! Every scenario is a checked-in, compile-checked [`ScenarioSpec`]: the
//! invocation drives a **real surface** (the Rust API today; the CLI
//! in-process runner for the failure drill), the assertions ride the D-16
//! self-golden rig (certified rows) or structural predicates (standard
//! rows), and the log expectations assert on the structured event stream
//! the harness captures into each run's deterministic NDJSON artifact.
//!
//! ## Seeded classes (fm-fjq)
//!
//! - **RENDER-MATRIX** — a Circle+Tex-label scene through the real native
//!   sinks × {y4m, PNG-sequence} × the CLI's quality presets
//!   (`-l` 854×480 in the fast tier; `-m`/`--hd`/`--uhd` in the
//!   `FMN_E2E_FULL=1` tier, one frame per row for the nightly budget) ×
//!   {certified, standard} engine identities. Certified rows bit-lock every
//!   emitted artifact in the `e2e` suite (`Scope::Certified`); standard
//!   rows assert structure (frame count, geometry, exit code, emitted-file
//!   inventory). The CLI rows also cover ordered two-scene batch publication
//!   through the real Asupersync-backed composition root.
//! - **DETERMINISM DRILLS** — the same render twice in a row is
//!   byte-identical; the certified terminal frame is byte-identical at
//!   {1, 4} threads (fast tier) and at 16 threads (full tier) — PG-5's
//!   thread-invariance form pointed at a real scene render.
//! - **FAILURE-PATH DRILLS** — an unsupported TeX construct surfaces the
//!   precise named `TexError::Math` refusal; a corrupted cache entry is
//!   evicted and recomputed, never fatal; an invalid CLI flag combination
//!   (`-l -m`) exits 2 with the `quality-exclusive` rule identity through
//!   `fmn_cli`'s in-process runner.
//! - **LIFECYCLE DRILL** — construct → snapshot → transform → snapshot
//!   with geometry assertions (the `scene_goldens` lifecycle form), plus
//!   a journal round-trip: bytes out, bytes in, identical content hash,
//!   and `plan_replay` reusing exactly the non-barrier prefix; PG-5 binds its
//!   checked-in definition to a representative `{1, 4, 16}` certified render,
//!   while PG-6 warms and reuses the real frame arena/output buffer for a
//!   registered corpus scene and requires an equal digest with zero measured
//!   allocations. PG-6's full tier also renders the exact three-view UHD 3D
//!   gallery through the production solid/surface and renderer APIs and checks
//!   every frame against its certified lock.
//! - **LOG ASSERTIONS** — the preflight typeset fires before frame zero
//!   with the typeset count recorded; the segment purity classification
//!   is recorded for play (pure) and wait (stateful); the engine identity
//!   and the `ExecutionPlan` tuning provenance are recorded on
//!   reproducible-class runs.
//!
//! ## Doctrine
//!
//! Each feature bead registers at least one e2e scenario when it lands
//! user-visible behavior; the fast tier runs per-commit and the full
//! matrix is env-gated (`FMN_E2E_FULL=1`) for the nightly budget. The
//! The Python surface remains **Pending**. Studio is live through the
//! production CLI composition root and has a registered frame scenario;
//! the CLI crate separately proves the subprocess supervisor boundary.
//! Every seeded class carries one deliberately-injected
//! regression drill (`RegressionKind`): the runner drives the scenario
//! red, and the repro bundle plus log artifact must appear.

use fmn_anim::{FramePacket, Timeline};
use fmn_conformance::e2e::{
    self, Assertion, FieldPred, Invocation, LogEvent, LogExpect, RegressionKind, RunCtx,
    RunOutcome, Runner, ScenarioClass, ScenarioError, ScenarioSpec, Status, Surface, Tier,
};
use fmn_conformance::golden::Scope;
use fmn_conformance::perf_pg5::{PG5_DIRECT_THREADS, Pg5Definition, pg5_identity};
use fmn_conformance::perf_pg6::{PG6_THREADS, Pg6Definition, pg6_identity};
use fmn_conformance::perf_pg6_peak::{
    PG6_PEAK_CASE_COUNT, PG6_PEAK_HEIGHT, PG6_PEAK_WIDTH, Pg6PeakDefinition,
    render_locked_gallery_once,
};
use fmn_conformance::scene_goldens::{self, TILING};
use fmn_core::color::Srgb;
use fmn_core::constants::{BLUE_C, WHITE};
use fmn_core::rng::RngRoot;
use fmn_frame::convert::{rgba_to_nv12, rgba16f_to_rgba8};
use fmn_frame::{ChromaSiting, ColorRange, FrameBuffer, FrameLayout, PixelFormat};
use fmn_hash::sha256;
use fmn_library::Circle;
use fmn_library::style::VStyle;
use fmn_library::vmobject::VMobject;
use fmn_mobject::Stage;
use fmn_mobject::animate::AnimateArgs;
use fmn_output::sinks::{
    NativeArtifactKind, NativeArtifactReport, PngSink, PngSinkConfig, PngTarget, SinkLimits,
    Y4mSink, Y4mSinkConfig,
};
use fmn_output::{FrameSink, ManifestMode, ProvenanceManifest, SinkWrite};
use fmn_platform::fs::FileSystem;
use fmn_platform::topology::HardwareTopology;
use fmn_render::FrameArena;
use fmn_render::bin::{Binning, ScreenMap, Viewport};
use fmn_render::engine::{
    EngineIdentity, EngineKind, FrameConfig, FrameJob, encode_frame, frame_digest,
};
use fmn_render::fill::MonoTable;
use fmn_render::plan::RenderPlan;
use fmn_runtime::{
    ExecutionPlan, OutputPixelFormat, PlanRequest, RenderIntent, SurfaceSpec, TuningSource,
};
use fmn_scene::journal::{CommandKind, CommandRecord, EffectClass, Entry, Journal, plan_replay};
use fmn_scene::{
    CaptureReason, IntegrationError, PlayOverrides, RuntimeConfig, Scene, SceneError, SceneProgram,
    SceneSink,
};
use fmn_tex::TexError;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

// ---------------------------------------------------------------------------
// Shared constants
// ---------------------------------------------------------------------------

/// Scene frame rate for every e2e render: small, exact, and identical
/// across the matrix so frame counts are a function of the scenario, not
/// the preset.
const FPS: u32 = 8;

/// The Reference's frame height in scene units (`sizes.frame_height`).
const FRAME_HEIGHT_UNITS: f64 = 8.0;

/// The e2e golden suite: one lock shared by the certified matrix.
const E2E_SUITE: &str = "e2e";

/// The full-tier opt-in environment gate (nightly budget, R22).
const FULL_TIER_ENV: &str = "FMN_E2E_FULL";

/// One CLI quality preset as geometry: the flag name, its default-config
/// resolution (`fmn-config`'s `resolution_options`), and the tier the row
/// runs in. Scale follows the Reference rule `height / frame_height`.
#[derive(Clone, Copy)]
struct Preset {
    flag: &'static str,
    width: u32,
    height: u32,
    tier: Tier,
}

const PRESETS: &[Preset] = &[
    Preset {
        flag: "low",
        width: 854,
        height: 480,
        tier: Tier::Fast,
    },
    Preset {
        flag: "medium",
        width: 1280,
        height: 720,
        tier: Tier::Full,
    },
    Preset {
        flag: "hd",
        width: 1920,
        height: 1080,
        tier: Tier::Full,
    },
    Preset {
        flag: "uhd",
        width: 3840,
        height: 2160,
        tier: Tier::Full,
    },
];

/// The native sinks the matrix covers.
#[derive(Clone, Copy, PartialEq, Eq)]
enum SinkKind {
    Y4m,
    PngSequence,
}

/// Process-unique suffix for on-disk sink destinations: PNG sequence
/// directories are claimed no-clobber, so rerunning a scenario in one
/// process must never collide with its own previous claim.
static RUN_SUFFIX: AtomicU64 = AtomicU64::new(0);

// ---------------------------------------------------------------------------
// Small stable-name mappers (event field values are the harness's contract)
// ---------------------------------------------------------------------------

fn engine_kind_name(kind: EngineKind) -> &'static str {
    match kind {
        EngineKind::CertifiedCpu => "certified_cpu",
        EngineKind::FastCpu => "fast_cpu",
        EngineKind::Metal => "metal",
        EngineKind::Cuda => "cuda",
    }
}

fn tuning_source_name(source: TuningSource) -> &'static str {
    match source {
        TuningSource::CertifiedProfile => "certified_profile",
        TuningSource::StandardBaseline => "standard_baseline",
        TuningSource::StandardAutotuneCache => "standard_autotune_cache",
    }
}

fn fail(message: impl Into<String>) -> ScenarioError {
    ScenarioError::new(message.into())
}

/// [`Assertion::Structural`] `CounterEq` as data.
fn counter_eq(name: &'static str, value: u64) -> Assertion {
    Assertion::Structural(e2e::StructuralAssert::CounterEq(name, value))
}

/// [`Assertion::Structural`] `CounterGe` as data.
fn counter_ge(name: &'static str, bound: u64) -> Assertion {
    Assertion::Structural(e2e::StructuralAssert::CounterGe(name, bound))
}

/// Event-field spelling for booleans, so `FieldPred::StrEq` assertions
/// compare against the recorded string form with no serialization guesswork.
const fn truth(value: bool) -> &'static str {
    if value { "true" } else { "false" }
}

// ---------------------------------------------------------------------------
// The e2e scene: Circle + Tex label, one animated shift, one wait
// ---------------------------------------------------------------------------

/// Build the Tex label mobject through the corpus's bundled engine.
fn tex_label(source: &'static str, font_size: f64) -> Result<VMobject, ScenarioError> {
    fmn_library::Tex::new(source)
        .font_size(font_size)
        .build(&scene_goldens::corpus().tex)
        .map(|built| built.vmob)
        .map_err(|error| fail(format!("tex label {source:?} typesets: {error}")))
}

/// The render-matrix / determinism scene program. Construction is a pure
/// function of the pre-built label and the scene seed; one play (pure
/// shift, 0.25 s ⇒ 2 frames at 8 fps) and one wait (0.125 s ⇒ 1 frame)
/// give a three-frame sequence with motion.
struct MatrixProgram {
    label: Option<VMobject>,
    wait_only: bool,
}

impl SceneProgram for MatrixProgram {
    fn name(&self) -> &str {
        "e2e_render_matrix"
    }

    fn construct(&mut self, scene: &mut Scene, sink: &mut dyn SceneSink) -> Result<(), SceneError> {
        let circle = scene.add_mobject(Circle::new().radius(0.9))?;
        scene
            .stage_mut()
            .set_fill(circle, Some(BLUE_C), Some(0.35), Some(0.0), true);
        scene
            .stage_mut()
            .set_stroke(circle, Some(WHITE), Some(2.0), Some(0.95), None, true);

        let label = self.label.take().ok_or(SceneError::InvalidLifecycle(
            "matrix program constructed twice",
        ))?;
        let label = scene.add_mobject(label)?;
        scene.stage_mut().shift(label, [0.0, -0.15, 0.0]);

        if !self.wait_only {
            let builder = circle
                .animate()
                .set_anim_args(AnimateArgs {
                    run_time: Some(0.25),
                    rate_func: Some(fmn_core::rate::linear),
                    ..AnimateArgs::default()
                })
                .map_err(|error| SceneError::Animation(error.into()))?
                .shift([0.6, 0.2, 0.0])
                .map_err(|error| SceneError::Animation(error.into()))?;
            let animation = fmn_anim::prepare_animation(builder, scene.stage_mut())?;
            scene.play(vec![animation], PlayOverrides::default(), sink)?;
        }
        scene.wait(Some(0.125), sink)?;
        Ok(())
    }
}

/// A sink that freezes each captured packet for the render stage.
#[derive(Default)]
struct PacketSink {
    packets: Vec<FramePacket>,
}

impl SceneSink for PacketSink {
    fn capture(
        &mut self,
        _reason: CaptureReason,
        packet: FramePacket,
    ) -> Result<(), IntegrationError> {
        self.packets.push(packet);
        Ok(())
    }
}

/// Run the matrix scene and return its frozen packets.
fn run_matrix_scene(seed: u64, wait_only: bool) -> Result<Vec<FramePacket>, ScenarioError> {
    let label = tex_label(r"\odot", 40.0)?;
    let mut scene = Scene::new(
        RuntimeConfig {
            fps: FPS,
            ..RuntimeConfig::default()
        },
        seed,
    )
    .map_err(|error| fail(format!("scene constructs: {error}")))?;
    let mut program = MatrixProgram {
        label: Some(label),
        wait_only,
    };
    let mut sink = PacketSink::default();
    scene
        .run(&mut program, &mut sink)
        .map_err(|error| fail(format!("scene runs: {error}")))?;
    Ok(sink.packets)
}

/// The frame configuration for one preset: the Reference's `#333333`
/// background, origin at frame center, scale `height / 8`.
fn preset_frame_config(preset: &Preset) -> FrameConfig {
    FrameConfig::new(
        Viewport {
            width: preset.width,
            height: preset.height,
        },
        ScreenMap {
            scale: f64::from(preset.height) / FRAME_HEIGHT_UNITS,
            origin: [
                f64::from(preset.width) / 2.0,
                f64::from(preset.height) / 2.0,
            ],
        },
        Srgb::from_rgb8(0x33, 0x33, 0x33).to_linear(1.0),
    )
}

/// Render one frozen packet through an explicit engine identity.
fn render_packet(
    packet: &FramePacket,
    identity: EngineIdentity,
    config: FrameConfig,
    threads: usize,
) -> Result<FrameBuffer, ScenarioError> {
    let stage = packet.materialize_stage();
    let mut plan = RenderPlan::new();
    let revision = u64::try_from(packet.frame_index())
        .map_err(|_| fail("negative frame index reached the renderer"))?;
    plan.sync(&stage, revision)
        .map_err(|error| fail(format!("render plan accepts packet geometry: {error}")))?;
    let mono = MonoTable::build(&plan, config.map)
        .map_err(|error| fail(format!("monotone table accepts packet geometry: {error}")))?;
    let mut binning = Binning::build(&plan, config.viewport, TILING, config.map)
        .expect("bounded conformance binning");
    binning
        .prune_occluded(&plan)
        .map_err(|error| fail(format!("occlusion pruning matches the plan: {error}")))?;
    FrameJob::with_identity(&plan, &mono, &binning, config, identity)
        .map_err(|error| fail(format!("frame job matches the plan: {error}")))?
        .render(threads)
        .map_err(|error| fail(format!("engine renders the frame: {error}")))
}

/// Convert a certified linear-light frame into the sink's negotiated
/// pixel format.
fn convert_for_sink(
    frame: &FrameBuffer,
    sink: SinkKind,
    width: u32,
    height: u32,
) -> Result<FrameBuffer, ScenarioError> {
    let mut rgba8 = FrameBuffer::new(
        FrameLayout::tight(PixelFormat::Rgba8, width, height)
            .map_err(|error| fail(format!("rgba8 layout: {error}")))?,
    );
    rgba16f_to_rgba8(frame, &mut rgba8)
        .map_err(|error| fail(format!("rgba16f → rgba8: {error}")))?;
    match sink {
        SinkKind::PngSequence => Ok(rgba8),
        SinkKind::Y4m => {
            let mut nv12 = FrameBuffer::new(
                FrameLayout::tight(PixelFormat::Nv12, width, height)
                    .map_err(|error| fail(format!("nv12 layout: {error}")))?,
            );
            // The ordinary video form: BT.709 limited range, left-sited
            // chroma — matching the C420mpeg2 container tag.
            rgba_to_nv12(&rgba8, &mut nv12, ColorRange::Limited, ChromaSiting::Left)
                .map_err(|error| fail(format!("rgba8 → nv12: {error}")))?;
            Ok(nv12)
        }
    }
}

/// The scenario-private output directory under the target tmpdir.
fn scenario_dir(scenario: &str) -> Result<PathBuf, ScenarioError> {
    let base =
        std::env::var_os("CARGO_TARGET_TMPDIR").map_or_else(std::env::temp_dir, PathBuf::from);
    let suffix = RUN_SUFFIX.fetch_add(1, Ordering::Relaxed);
    let dir = base
        .join("fmn_e2e")
        .join(format!("{scenario}.{}.{}", std::process::id(), suffix));
    std::fs::create_dir_all(&dir)
        .map_err(|error| fail(format!("create {}: {error}", dir.display())))?;
    Ok(dir)
}

fn sink_limits(frames: u64) -> Result<SinkLimits, ScenarioError> {
    SinkLimits::new(16, 1 << 30, 1 << 34, 1 << 34)
        .and_then(|limits| limits.requiring_exact_frames(frames))
        .map_err(|error| fail(format!("sink limits: {error}")))
}

/// Drive the real native y4m sink over the rendered frames and read the
/// published artifact back from disk.
fn publish_y4m(
    dir: &std::path::Path,
    preset: &Preset,
    frames: &[FrameBuffer],
) -> Result<(String, Vec<u8>, NativeArtifactReport), ScenarioError> {
    let destination = dir.join("out.y4m");
    let mut sink = Y4mSink::new(
        Arc::new(fmn_platform::fs::StdFs),
        Y4mSinkConfig {
            destination: destination.clone(),
            width: preset.width,
            height: preset.height,
            fps: (FPS, 1),
            colorspace: fmn_codec::Y4mColorspace::C420Mpeg2,
            first_sequence: 0,
            limits: sink_limits(frames.len() as u64)?,
            profile: None,
        },
    )
    .map_err(|error| fail(format!("y4m sink constructs: {error}")))?;
    let receipt = sink.receipt();
    for (index, frame) in frames.iter().enumerate() {
        match sink.write_frame(index as u64, frame) {
            Ok(SinkWrite::Consumed) => {}
            Ok(SinkWrite::WouldBlock) => {
                return Err(fail("reliable y4m sink reported WouldBlock"));
            }
            Err(error) => return Err(fail(format!("y4m sink write: {error}"))),
        }
    }
    sink.finish()
        .map_err(|error| fail(format!("y4m sink finish: {error}")))?;
    let report = receipt
        .take()
        .map_err(|error| fail(format!("y4m receipt: {error}")))?;
    let bytes = std::fs::read(&destination)
        .map_err(|error| fail(format!("read {}: {error}", destination.display())))?;
    Ok(("out.y4m".to_string(), bytes, report))
}

/// The published PNG-sequence payload: `(file name, bytes)` pairs in
/// sequence order, plus the sink's completion report.
type PublishedSequence = (Vec<(String, Vec<u8>)>, NativeArtifactReport);

/// Drive the real canonical PNG-sequence sink and read every published
/// frame back from disk (file order = sequence order).
fn publish_png_sequence(
    dir: &std::path::Path,
    preset: &Preset,
    frames: &[FrameBuffer],
    certified: bool,
) -> Result<PublishedSequence, ScenarioError> {
    let directory = dir.join("frames");
    let mut sink = PngSink::new(
        Arc::new(fmn_platform::fs::StdFs),
        PngSinkConfig {
            target: PngTarget::Sequence {
                directory: directory.clone(),
                stem: "frame".to_string(),
                digits: 4,
            },
            width: preset.width,
            height: preset.height,
            first_sequence: 0,
            compression: if certified {
                fmn_codec::CompressionLevel::Best
            } else {
                fmn_codec::CompressionLevel::Default
            },
            threads: 1,
            limits: sink_limits(frames.len() as u64)?,
            profile: None,
        },
    )
    .map_err(|error| fail(format!("png sink constructs: {error}")))?;
    let receipt = sink.receipt();
    for (index, frame) in frames.iter().enumerate() {
        match sink.write_frame(index as u64, frame) {
            Ok(SinkWrite::Consumed) => {}
            Ok(SinkWrite::WouldBlock) => {
                return Err(fail("reliable png sink reported WouldBlock"));
            }
            Err(error) => return Err(fail(format!("png sink write: {error}"))),
        }
    }
    sink.finish()
        .map_err(|error| fail(format!("png sink finish: {error}")))?;
    let report = receipt
        .take()
        .map_err(|error| fail(format!("png receipt: {error}")))?;
    let mut files: Vec<PathBuf> = std::fs::read_dir(&directory)
        .map_err(|error| fail(format!("list {}: {error}", directory.display())))?
        .map(|entry| entry.map(|entry| entry.path()))
        .collect::<Result<_, _>>()
        .map_err(|error| fail(format!("read dir entry: {error}")))?;
    files.sort();
    let mut published = Vec::new();
    for file in files {
        if file.extension().is_some_and(|ext| ext == "png") {
            let name = file
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .ok_or_else(|| fail("png sequence child has no file name"))?;
            let bytes = std::fs::read(&file)
                .map_err(|error| fail(format!("read {}: {error}", file.display())))?;
            published.push((name, bytes));
        }
    }
    Ok((published, report))
}

// ---------------------------------------------------------------------------
// Render-matrix invocation
// ---------------------------------------------------------------------------

/// One matrix row: run the scene, render every packet under the row's
/// engine identity, convert, publish through the real native sink, and
/// return the emitted files as artifacts plus the structural counters and
/// the machinery events (engine identity, plan decision, per-frame render,
/// per-file emission).
fn render_matrix_run(
    ctx: &mut RunCtx,
    preset: &Preset,
    sink_kind: SinkKind,
    certified: bool,
    stem: &str,
) -> Result<RunOutcome, ScenarioError> {
    ctx.set_fps((FPS, 1));
    let identity = if certified {
        EngineIdentity::certified()
    } else {
        EngineIdentity::fast()
    };
    let threads = if certified { 1 } else { 4 };
    let config = preset_frame_config(preset);

    // The ExecutionPlan decision for this row, derived over a fixed
    // synthetic topology so the recorded decision is a function of the
    // request, never of the host running the test.
    let topology = HardwareTopology::fallback(8);
    let request = if certified {
        PlanRequest::certified(
            RenderIntent::Offline,
            SurfaceSpec::lumen(preset.width, preset.height),
            OutputPixelFormat::Rgba8,
        )
    } else {
        PlanRequest::standard(
            RenderIntent::Offline,
            SurfaceSpec::lumen(preset.width, preset.height),
            OutputPixelFormat::Rgba8,
        )
    };
    let plan = ExecutionPlan::derive(request, &topology, None)
        .map_err(|error| fail(format!("execution plan derives: {error}")))?;
    ctx.event(
        LogEvent::new(e2e::spans::EXECUTION_PLAN)
            .field(
                "determinism",
                if certified { "certified" } else { "standard" },
            )
            .field("engine", engine_kind_name(identity.engine))
            .field("tuning_source", tuning_source_name(plan.tuning_source))
            .field("fine_tile", u64::from(plan.fine_tile))
            .field("macro_tile", u64::from(plan.macro_tile)),
    );
    ctx.event(
        LogEvent::new(e2e::spans::ENGINE)
            .field("engine", engine_kind_name(identity.engine))
            .field("reproducible", if certified { "true" } else { "false" })
            .field("renderer_version", u64::from(identity.renderer_version)),
    );

    let packets = run_matrix_scene(ctx.seed, preset.tier == Tier::Full)?;
    ctx.event(
        LogEvent::new(e2e::spans::SCENE_CONSTRUCT)
            .field("scene", "e2e_render_matrix")
            .field("seed", ctx.seed)
            .field("frames", packets.len() as u64),
    );

    let mut converted = Vec::with_capacity(packets.len());
    for (index, packet) in packets.iter().enumerate() {
        let frame = render_packet(packet, identity, config, threads)?;
        converted.push(convert_for_sink(
            &frame,
            sink_kind,
            preset.width,
            preset.height,
        )?);
        ctx.event(
            LogEvent::new(e2e::spans::RENDER_FRAME)
                .field("frame", index as u64)
                .field("engine", engine_kind_name(identity.engine)),
        );
    }

    let dir = scenario_dir("render_matrix")?;
    let mut outcome = RunOutcome::ok();
    let mut total_bytes = 0_u64;
    match sink_kind {
        SinkKind::Y4m => {
            let (name, bytes, report) = publish_y4m(&dir, preset, &converted)?;
            if report.kind != NativeArtifactKind::Y4m {
                return Err(fail("y4m receipt reports the wrong artifact kind"));
            }
            total_bytes += bytes.len() as u64;
            ctx.event(
                LogEvent::new(e2e::spans::EMIT)
                    .field("file", name.as_str())
                    .field("bytes", bytes.len() as u64),
            );
            outcome = outcome.with_artifact(stem, bytes);
        }
        SinkKind::PngSequence => {
            let (published, report) = publish_png_sequence(&dir, preset, &converted, certified)?;
            if report.kind != NativeArtifactKind::PngSequence {
                return Err(fail("png receipt reports the wrong artifact kind"));
            }
            for (index, (file, bytes)) in published.into_iter().enumerate() {
                total_bytes += bytes.len() as u64;
                ctx.event(
                    LogEvent::new(e2e::spans::EMIT)
                        .field("file", file.as_str())
                        .field("bytes", bytes.len() as u64),
                );
                let artifact = format!("{stem}.{index:04}");
                outcome = outcome.with_artifact(&artifact, bytes);
            }
        }
    }

    let frames = converted.len() as u64;
    ctx.counter(e2e::counters::FRAMES_RASTERIZED, frames);
    ctx.counter(e2e::counters::FRAMES_CONVERTED, frames);
    ctx.counter(e2e::counters::FRAMES_EMITTED, frames);
    outcome = outcome
        .with_counter("frames", frames)
        .with_counter("width", u64::from(preset.width))
        .with_counter("height", u64::from(preset.height))
        .with_counter("emitted_bytes", total_bytes);
    Ok(outcome)
}

fn matrix_spec(
    preset: &Preset,
    sink_kind: SinkKind,
    certified: bool,
    drill_name: Option<&'static str>,
) -> ScenarioSpec {
    let base: &'static str = match (preset.flag, sink_kind, certified) {
        ("low", SinkKind::Y4m, true) => "render_matrix.low.y4m.certified.v1",
        ("low", SinkKind::Y4m, false) => "render_matrix.low.y4m.standard.v1",
        ("low", SinkKind::PngSequence, true) => "render_matrix.low.png_seq.certified.v1",
        ("low", SinkKind::PngSequence, false) => "render_matrix.low.png_seq.standard.v1",
        ("medium", SinkKind::Y4m, true) => "render_matrix.medium.y4m.certified.v1",
        ("medium", SinkKind::Y4m, false) => "render_matrix.medium.y4m.standard.v1",
        ("medium", SinkKind::PngSequence, true) => "render_matrix.medium.png_seq.certified.v1",
        ("medium", SinkKind::PngSequence, false) => "render_matrix.medium.png_seq.standard.v1",
        ("hd", SinkKind::Y4m, true) => "render_matrix.hd.y4m.certified.v1",
        ("hd", SinkKind::Y4m, false) => "render_matrix.hd.y4m.standard.v1",
        ("hd", SinkKind::PngSequence, true) => "render_matrix.hd.png_seq.certified.v1",
        ("hd", SinkKind::PngSequence, false) => "render_matrix.hd.png_seq.standard.v1",
        ("uhd", SinkKind::Y4m, true) => "render_matrix.uhd.y4m.certified.v1",
        ("uhd", SinkKind::Y4m, false) => "render_matrix.uhd.y4m.standard.v1",
        ("uhd", SinkKind::PngSequence, true) => "render_matrix.uhd.png_seq.certified.v1",
        ("uhd", SinkKind::PngSequence, false) => "render_matrix.uhd.png_seq.standard.v1",
        _ => "render_matrix.unknown.v1",
    };
    // The drill row gets its own scenario name (its log and bundle files)
    // but reuses the base row's artifact stem: the harness evaluates a
    // golden drill in forced check mode against the base row's blessed
    // lock entries, so the injected flip lands as self-golden drift —
    // never as a blessable new entry.
    let name: &'static str = drill_name.unwrap_or(base);
    let stem: &'static str = base;

    let expected_frames: u64 = match preset.tier {
        Tier::Fast => 3,
        Tier::Full => 1,
    };
    let preset_copy = *preset;
    let invocation = Invocation::new(move |ctx: &mut RunCtx| {
        render_matrix_run(ctx, &preset_copy, sink_kind, certified, stem)
    });

    let mut assertions = vec![
        Assertion::ExitCode(0),
        counter_eq("frames", expected_frames),
        counter_eq("width", u64::from(preset.width)),
        counter_eq("height", u64::from(preset.height)),
    ];
    if certified {
        // Certified rows bit-lock every emitted artifact against the
        // shared certified-matrix lock (D-16).
        assertions.push(Assertion::GoldenLock {
            suite: E2E_SUITE,
            scope: Scope::Certified,
        });
    } else {
        // Standard rows assert the emitted-file inventory instead: the
        // fast engine's bits may legitimately differ across SIMD tiers.
        let inventory = match sink_kind {
            SinkKind::Y4m => vec![stem.to_string()],
            SinkKind::PngSequence => (0..expected_frames)
                .map(|index| format!("{stem}.{index:04}"))
                .collect(),
        };
        assertions.push(Assertion::FileInventory(inventory));
    }
    // The runner's own NDJSON log contract is under test on every run.
    assertions.push(Assertion::NdjsonSchema);

    ScenarioSpec {
        name,
        class: ScenarioClass::RenderMatrix,
        surface: Surface::RustApi,
        tier: preset.tier,
        invocation,
        assertions,
        logs: vec![
            LogExpect::span_present(
                e2e::spans::ENGINE,
                vec![
                    FieldPred::str_eq("engine", engine_kind_name_str(certified)),
                    FieldPred::str_eq("reproducible", truth(certified)),
                ],
            ),
            LogExpect::span_present(
                e2e::spans::EXECUTION_PLAN,
                vec![FieldPred::str_eq(
                    "tuning_source",
                    if certified {
                        "certified_profile"
                    } else {
                        "standard_baseline"
                    },
                )],
            ),
            LogExpect::event_order(e2e::spans::SCENE_CONSTRUCT, e2e::spans::RENDER_FRAME),
            LogExpect::counter_ge(e2e::counters::FRAMES_EMITTED, expected_frames),
        ],
        regression: None,
    }
}

const fn engine_kind_name_str(certified: bool) -> &'static str {
    if certified {
        "certified_cpu"
    } else {
        "fast_cpu"
    }
}

// ---------------------------------------------------------------------------
// Determinism drills
// ---------------------------------------------------------------------------

/// Twice-in-a-row: the identical scene rendered twice within one run must
/// produce byte-identical artifacts (PG-5's reproducibility form).
fn determinism_repeat_run(ctx: &mut RunCtx) -> Result<RunOutcome, ScenarioError> {
    ctx.set_fps((FPS, 1));
    let preset = PRESETS[0];
    let identity = EngineIdentity::certified();
    let config = preset_frame_config(&preset);

    let mut runs = Vec::with_capacity(2);
    for run in 0..2_u64 {
        let packets = run_matrix_scene(ctx.seed, false)?;
        let mut encoded = Vec::with_capacity(packets.len());
        for packet in &packets {
            let frame = render_packet(packet, identity, config, 1)?;
            encoded.push(
                encode_frame(&frame).map_err(|error| fail(format!("frame encodes: {error}")))?,
            );
        }
        ctx.event(
            LogEvent::new(e2e::spans::RENDER_FRAME)
                .field("run", run)
                .field("frames", encoded.len() as u64),
        );
        runs.push(encoded.concat());
    }
    let equal = runs[0] == runs[1];
    ctx.counter("repeat_byte_equal", u64::from(equal));
    if !equal {
        return Err(fail(
            "twice-in-a-row certified renders diverged byte-for-byte",
        ));
    }
    Ok(RunOutcome::ok()
        .with_artifact("determinism.repeat.v1", runs[0].clone())
        .with_counter("frames", 3)
        .with_counter("repeat_byte_equal", 1))
}

/// Thread-count invariance: the certified terminal frame at {1, 4}
/// threads (and 16 in the full tier) is byte-identical — PG-5's form.
fn determinism_threads_run(
    ctx: &mut RunCtx,
    max_threads: usize,
) -> Result<RunOutcome, ScenarioError> {
    ctx.set_fps((FPS, 1));
    let preset = PRESETS[0];
    let identity = EngineIdentity::certified();
    let config = preset_frame_config(&preset);

    let packets = run_matrix_scene(ctx.seed, false)?;
    let terminal = packets
        .last()
        .ok_or_else(|| fail("scene produced no terminal packet"))?;
    let scalar = render_packet(terminal, identity, config, 1)?;
    let scalar_doc = encode_frame(&scalar).map_err(|error| fail(format!("encode: {error}")))?;
    let mut equal = true;
    for threads in [4, 16]
        .into_iter()
        .filter(|threads| *threads <= max_threads)
    {
        let threaded = render_packet(terminal, identity, config, threads)?;
        let threaded_doc =
            encode_frame(&threaded).map_err(|error| fail(format!("encode: {error}")))?;
        ctx.event(
            LogEvent::new(e2e::spans::RENDER_FRAME)
                .field("threads", threads as u64)
                .field("byte_equal", truth(threaded_doc == scalar_doc)),
        );
        equal &= threaded_doc == scalar_doc;
    }
    ctx.counter("thread_byte_equal", u64::from(equal));
    if !equal {
        return Err(fail(format!(
            "certified terminal frame drifted at up to {max_threads} threads"
        )));
    }
    Ok(RunOutcome::ok()
        .with_counter("frames", 3)
        .with_counter("max_threads", max_threads as u64)
        .with_counter("thread_byte_equal", 1))
}

// ---------------------------------------------------------------------------
// Failure-path drills
// ---------------------------------------------------------------------------

/// Unsupported TeX construct: `\dx` is a named, tier-tagged refusal
/// (fmd-math T2; `\substack` was the example until it graduated under
/// fm-j5t) that must surface verbatim through the e2e runner — never a
/// blank render, never a panic.
fn failure_tex_run(ctx: &mut RunCtx) -> Result<RunOutcome, ScenarioError> {
    let engine = &scene_goldens::corpus().tex;
    let source = r"\dx";
    match engine.typeset(fmn_tex::Mode::Math(fmd_math::Style::Text), source) {
        Ok(_) => Err(fail(
            "unsupported construct \\dx typeset successfully — the named refusal is gone",
        )),
        Err(error) => {
            let message = error.to_string();
            let is_math = matches!(error, TexError::Math(_));
            let names_construct = message.contains("dx");
            let named_refusal = message.contains("not yet supported");
            ctx.event(
                LogEvent::new("e2e.failure")
                    .field(
                        "error_kind",
                        if is_math { "TexError::Math" } else { "other" },
                    )
                    .field("names_construct", truth(names_construct))
                    .field("named_refusal", truth(named_refusal)),
            );
            ctx.counter(
                "error_named",
                u64::from(is_math && names_construct && named_refusal),
            );
            if !(is_math && names_construct && named_refusal) {
                return Err(fail(format!(
                    "unsupported-construct refusal lost its precise identity: {message}"
                )));
            }
            Ok(RunOutcome::ok()
                .with_counter("error_named", 1)
                .with_counter("exit_code", 0))
        }
    }
}

/// Corrupt cache: a flipped byte in a store object is detected, evicted,
/// and reported as a miss — and the read-through path recomputes. The
/// whole drill runs in the deterministic lab (VirtualFs + FakeClock), so
/// the corruption is a function of the scenario, never the host disk.
fn failure_cache_run(ctx: &mut RunCtx) -> Result<RunOutcome, ScenarioError> {
    use fmn_cache::{CacheKey, NamespacePolicy, Store, StoreConfig};
    use fmn_platform::clock::FakeClock;
    use fmn_platform::fs::VirtualFs;

    let fs = Arc::new(VirtualFs::new());
    let clock = Arc::new(FakeClock::new());
    let cache_root = if cfg!(windows) {
        r"C:\e2e-cache"
    } else {
        "/e2e-cache"
    };
    let store = Store::open(fs.clone(), clock, cache_root, StoreConfig::default())
        .map_err(|error| fail(format!("store opens: {error}")))?;
    let namespace = store
        .namespace(
            "e2e",
            1,
            NamespacePolicy {
                ceiling_bytes: None,
            },
        )
        .map_err(|error| fail(format!("namespace opens: {error}")))?;
    let key = CacheKey::of_content(b"e2e.corruption-drill.victim");
    namespace
        .put(&key, b"good value")
        .map_err(|error| fail(format!("cache put: {error}")))?;

    // Find the one object file and flip a byte in the middle.
    let mut objects = Vec::new();
    collect_virtual_objects(fs.as_ref(), std::path::Path::new(cache_root), &mut objects);
    if objects.len() != 1 {
        return Err(fail(format!(
            "expected exactly one cache object, found {}",
            objects.len()
        )));
    }
    let victim = &objects[0];
    let mut bytes = fs
        .read(victim)
        .map_err(|error| fail(format!("read cache object: {error}")))?;
    let mid = bytes.len() / 2;
    bytes[mid] ^= 0x40;
    fs.write_atomic(victim, &bytes)
        .map_err(|error| fail(format!("corrupt cache object: {error}")))?;

    // Detection ⇒ eviction ⇒ miss, never an error, never a panic.
    let after = namespace
        .get(&key)
        .map_err(|error| fail(format!("corrupt lookup escalated to an error: {error}")))?;
    let evicted = !fs.exists(victim);
    let miss = after.is_none();
    ctx.event(
        LogEvent::new("e2e.failure")
            .field("cache_corrupt_miss", truth(miss))
            .field("cache_corrupt_evicted", truth(evicted)),
    );

    // The read-through path recomputes and repopulates cleanly.
    let recomputed: Vec<u8> = match namespace.get_or_compute(&key, || {
        Ok::<_, std::convert::Infallible>(b"good value".to_vec())
    }) {
        Ok(value) => value,
        Err(error) => match error {},
    };
    let recovered = recomputed == b"good value"
        && namespace
            .get(&key)
            .map_err(|error| fail(format!("post-recovery get: {error}")))?
            .as_deref()
            == Some(&b"good value"[..]);
    ctx.counter("cache_corrupt_miss", u64::from(miss));
    ctx.counter("cache_recovery_ok", u64::from(evicted && recovered));
    if !(miss && evicted && recovered) {
        return Err(fail(
            "corrupt cache entry was not (detected, evicted, recomputed)",
        ));
    }
    Ok(RunOutcome::ok()
        .with_counter("cache_corrupt_miss", 1)
        .with_counter("cache_recovery_ok", 1))
}

fn collect_virtual_objects(
    fs: &fmn_platform::fs::VirtualFs,
    dir: &std::path::Path,
    out: &mut Vec<PathBuf>,
) {
    if let Ok(children) = fs.list_dir(dir) {
        for child in children {
            if fs.read(&child).is_ok() {
                // Component-wise: Windows paths separate with backslashes.
                if child.components().any(|c| c.as_os_str() == "objects") {
                    out.push(child);
                }
            } else {
                collect_virtual_objects(fs, &child, out);
            }
        }
    }
}

/// Invalid CLI flag combination: `-l -m` is a usage error with the
/// `quality-exclusive` rule identity, exit code 2, one NDJSON line on
/// stdout, and an empty stderr — through fmn_cli's in-process runner
/// (never a subprocess). Parse errors never touch the host filesystem.
fn failure_cli_run(ctx: &mut RunCtx) -> Result<RunOutcome, ScenarioError> {
    let output = fmn_cli::run(["--robot", "-l", "-m"]);
    let single_line = output.stdout.lines().count() == 1 && !output.stdout.is_empty();
    let names_rule = output.stdout.contains("\"rule\":\"quality-exclusive\"");
    let names_exit = output.stdout.contains("\"exit_name\":\"usage\"");
    let clean_stderr = output.stderr.is_empty();
    ctx.event(
        LogEvent::new("e2e.failure")
            .field("rule", "quality-exclusive")
            .field("rule_named", truth(names_rule))
            .field("exit_named", truth(names_exit))
            .field("ndjson_single_line", truth(single_line))
            .field("stderr_empty", truth(clean_stderr)),
    );
    ctx.counter("cli_rule_named", u64::from(names_rule && names_exit));
    if output.code != 2 || !names_rule || !names_exit || !single_line || !clean_stderr {
        return Err(fail(format!(
            "quality-exclusive lost its named identity: code={} stdout={:?} stderr={:?}",
            output.code, output.stdout, output.stderr
        )));
    }
    Ok(RunOutcome::ok()
        .exit_code(2)
        .with_counter("cli_rule_named", 1))
}

/// Studio's first real Gauntlet row: select a shipped scene through the CLI
/// schema and compose its preview through the same worker renderer root used by
/// `fmn studio`. The CLI runtime-boundary suite separately proves that this
/// root survives the disposable-worker and loopback-host boundary.
fn studio_preview_run(ctx: &mut RunCtx) -> Result<RunOutcome, ScenarioError> {
    let invocation = fmn_cli::parse_args([
        "studio",
        "--no-browser",
        "--resolution",
        "96x54",
        "--fps",
        "8",
        "--threads",
        "1",
        fmn_cli::BUILTIN_SCENE_SOURCE,
        "circle_shift.v1",
    ])
    .map_err(|error| fail(format!("parse Studio scenario: {error}")))?;
    let fmn_cli::Invocation::Studio(command) = invocation else {
        return Err(fail("Studio scenario parsed to a different front door"));
    };
    let stream = fmn_cli::compose_studio_preview_frame(&fmn_platform::fs::StdFs, &command, 1)
        .map_err(|error| fail(format!("compose Studio preview: {error}")))?;
    stream
        .validate(fmn_studio::ProtocolLimits::default())
        .map_err(|error| fail(format!("validate Studio frame protocol: {error}")))?;
    let backend_is_stream = stream.render_backends.len() == 1
        && stream.render_backends[0].role() == fmn_scene::RenderBackendRole::FrameStream;
    let fmn_studio::FramePayload::Pipe { bytes, .. } = stream.payload else {
        return Err(fail(
            "Studio scenario did not return its bounded pipe frame",
        ));
    };
    let decoded = fmn_codec::decode_png(
        &bytes,
        &fmn_codec::PngLimits {
            max_pixels: u64::from(stream.width) * u64::from(stream.height),
            ..fmn_codec::PngLimits::default()
        },
    )
    .map_err(|error| fail(format!("decode Studio preview PNG: {error}")))?;
    let dimensions_match = decoded.width == 96 && decoded.height == 54;
    ctx.event(
        LogEvent::new("e2e.studio.preview")
            .field("scene", stream.scene.as_str())
            .field("frame", stream.frame_index)
            .field("encoding", "png")
            .field("backend_is_stream", truth(backend_is_stream))
            .field("dimensions_match", truth(dimensions_match)),
    );
    ctx.counter("studio_preview_frames", 1);
    ctx.counter("studio_png_dimensions", u64::from(dimensions_match));
    ctx.counter("studio_backend_journaled", u64::from(backend_is_stream));
    if stream.scene != "circle_shift.v1"
        || stream.frame_index != 1
        || !backend_is_stream
        || !dimensions_match
    {
        return Err(fail(format!(
            "Studio preview drifted: scene={:?} frame={} backend_stream={} dimensions={}x{}",
            stream.scene, stream.frame_index, backend_is_stream, decoded.width, decoded.height
        )));
    }
    Ok(RunOutcome::ok()
        .with_artifact("studio_frame.png", bytes)
        .with_counter("studio_preview_frames", 1)
        .with_counter("studio_png_dimensions", 1)
        .with_counter("studio_backend_journaled", 1))
}

/// The shipped CLI's first positive native-registration path: one program
/// selected from `@builtin`, rendered by Lumen, converted by fmn-frame, and
/// atomically published by Reel's ordered PNG-sequence sink.
fn cli_builtin_render_run(ctx: &mut RunCtx) -> Result<RunOutcome, ScenarioError> {
    let dir = scenario_dir("cli_builtin_render")?;
    let dir_text = dir
        .to_str()
        .ok_or_else(|| fail("CLI scenario output path is not UTF-8"))?;
    let output = fmn_cli::run([
        "--robot",
        "--format",
        "png_sequence",
        "--resolution",
        "96x54",
        "--fps",
        "8",
        "--threads",
        "1",
        "--video_dir",
        dir_text,
        fmn_cli::BUILTIN_SCENE_SOURCE,
        "circle_shift.v1",
    ]);
    let sequence = dir.join("circle_shift.v1");
    let complete = sequence.join("FMN_COMPLETE").is_file();
    let pngs = std::fs::read_dir(&sequence)
        .map_err(|error| fail(format!("list CLI PNG sequence: {error}")))?
        .filter_map(Result::ok)
        .filter(|entry| entry.path().extension().is_some_and(|ext| ext == "png"))
        .count();
    let signature = std::fs::read(sequence.join("frame_000000.png"))
        .map(|bytes| bytes.starts_with(b"\x89PNG\r\n\x1a\n"))
        .unwrap_or(false);
    let manifest_generation = dir.join("circle_shift.v1.manifest");
    let manifest = std::fs::read(manifest_generation.join("manifest.fmnp"))
        .ok()
        .and_then(|bytes| ProvenanceManifest::from_bytes(&bytes).ok());
    let manifest_complete = manifest_generation.join("FMN_COMPLETE").is_file();
    let manifest_valid = manifest.as_ref().is_some_and(|manifest| {
        manifest.mode == ManifestMode::Standard
            && manifest.outputs.len() == 1
            && (1..=10).all(|id| manifest.items.iter().any(|item| item.item_id == id))
    });
    let robot_record = output.stdout.lines().count() == 1
        && output.stdout.contains("\"kind\":\"render\"")
        && output.stdout.contains("\"source\":\"builtin\"")
        && output.stdout.contains("\"frames\":3")
        && output.stdout.contains("circle_shift.v1.manifest");
    ctx.event(
        LogEvent::new("e2e.cli.render")
            .field("source", "builtin")
            .field("format", "png_sequence")
            .field("complete", truth(complete))
            .field("png_signature", truth(signature))
            .field("manifest_complete", truth(manifest_complete))
            .field("manifest_valid", truth(manifest_valid))
            .field("robot_record", truth(robot_record)),
    );
    ctx.counter("cli_render_frames", pngs as u64);
    ctx.counter("cli_png_signature", u64::from(signature));
    ctx.counter(
        "cli_manifest",
        u64::from(manifest_complete && manifest_valid),
    );
    if output.code != 0
        || !output.stderr.is_empty()
        || !complete
        || pngs != 3
        || !signature
        || !manifest_complete
        || !manifest_valid
        || !robot_record
    {
        return Err(fail(format!(
            "native CLI render failed: code={} pngs={pngs} complete={complete} signature={signature} manifest_complete={manifest_complete} manifest_valid={manifest_valid} stdout={:?} stderr={:?}",
            output.code, output.stdout, output.stderr
        )));
    }
    Ok(RunOutcome::ok()
        .with_counter("cli_render_frames", 3)
        .with_counter("cli_png_signature", 1)
        .with_counter("cli_manifest", 1))
}

/// Count the ordinary scene schedule without touching Reel, then drive each
/// authored segment through its own real native sink.
fn cli_prerun_subdivide_run(ctx: &mut RunCtx) -> Result<RunOutcome, ScenarioError> {
    let dir = scenario_dir("cli_prerun_subdivide")?;
    let dir_text = dir
        .to_str()
        .ok_or_else(|| fail("CLI scenario output path is not UTF-8"))?;
    let output = fmn_cli::run([
        "--robot",
        "--prerun",
        "--subdivide",
        "--format",
        "y4m",
        "--resolution",
        "96x54",
        "--fps",
        "8",
        "--threads",
        "1",
        "--video_dir",
        dir_text,
        fmn_cli::BUILTIN_SCENE_SOURCE,
        "circle_shift.v1",
    ]);
    let records = output.stdout.lines().collect::<Vec<_>>();
    let prerun_record = records.first().is_some_and(|record| {
        record.contains("\"kind\":\"prerun\"")
            && record.contains("\"frames\":3")
            && record.contains("\"segments\":2")
    });
    let render_records = records.len() == 3
        && records.get(1).is_some_and(|record| {
            record.contains("\"subdivision\":0") && record.contains("\"frames\":2")
        })
        && records.get(2).is_some_and(|record| {
            record.contains("\"subdivision\":1") && record.contains("\"frames\":1")
        });
    let root = dir.join("circle_shift.v1");
    let mut rendered_frames = 0_usize;
    let mut complete = true;
    for index in 0..2 {
        let artifact = root.join(format!("{index:05}.y4m"));
        let bytes = std::fs::read(&artifact).unwrap_or_default();
        rendered_frames += bytes
            .windows(6)
            .filter(|window| *window == b"FRAME\n")
            .count();
        complete &= artifact
            .with_file_name(format!("{index:05}.y4m.manifest"))
            .join("FMN_COMPLETE")
            .is_file();
    }
    ctx.event(
        LogEvent::new("e2e.cli.prerun_subdivide")
            .field("prerun_record", truth(prerun_record))
            .field("render_records", truth(render_records))
            .field("complete", truth(complete))
            .field("rendered_frames", rendered_frames as u64),
    );
    ctx.counter("cli_prerun_frames", u64::from(prerun_record) * 3);
    ctx.counter("cli_subdivision_segments", u64::from(render_records) * 2);
    ctx.counter("cli_subdivision_frames", rendered_frames as u64);
    if output.code != 0
        || !output.stderr.is_empty()
        || !prerun_record
        || !render_records
        || !complete
        || rendered_frames != 3
    {
        return Err(fail(format!(
            "CLI prerun/subdivision failed: code={} rendered_frames={rendered_frames} complete={complete} stdout={:?} stderr={:?}",
            output.code, output.stdout, output.stderr
        )));
    }
    Ok(RunOutcome::ok()
        .with_counter("cli_prerun_frames", 3)
        .with_counter("cli_subdivision_segments", 2)
        .with_counter("cli_subdivision_frames", 3))
}

/// The batch front door over two real native registrations. Asupersync may
/// complete scene jobs in either order; the CLI contract reports them in the
/// request order and publishes each sequence atomically.
fn cli_batch_render_run(ctx: &mut RunCtx) -> Result<RunOutcome, ScenarioError> {
    let dir = scenario_dir("cli_batch_render")?;
    let artifact_dir = dir.join("artifacts");
    let manifest_dir = dir.join("manifests");
    std::fs::create_dir(&artifact_dir)
        .map_err(|error| fail(format!("create batch artifact directory: {error}")))?;
    std::fs::create_dir(&manifest_dir)
        .map_err(|error| fail(format!("create batch manifest directory: {error}")))?;
    let artifact_text = artifact_dir
        .to_str()
        .ok_or_else(|| fail("CLI batch output path is not UTF-8"))?;
    let manifest_text = manifest_dir
        .to_str()
        .ok_or_else(|| fail("CLI batch manifest path is not UTF-8"))?;
    let scenes = ["circle_shift.v1", "rectangle_shift.v1"];
    let output = fmn_cli::run([
        "batch",
        "--robot",
        "--format",
        "png_sequence",
        "--resolution",
        "96x54",
        "--fps",
        "8",
        "--threads",
        "1",
        "--max-scenes",
        "2",
        "--manifest-dir",
        manifest_text,
        "--video_dir",
        artifact_text,
        fmn_cli::BUILTIN_SCENE_SOURCE,
        scenes[0],
        scenes[1],
    ]);
    let mut frames = 0_usize;
    let mut complete = true;
    let mut signatures = true;
    let mut manifests = true;
    for scene in scenes {
        let sequence = artifact_dir.join(scene);
        complete &= sequence.join("FMN_COMPLETE").is_file();
        frames = frames.saturating_add(
            std::fs::read_dir(&sequence)
                .map_err(|error| fail(format!("list batch sequence {scene}: {error}")))?
                .filter_map(Result::ok)
                .filter(|entry| entry.path().extension().is_some_and(|ext| ext == "png"))
                .count(),
        );
        signatures &= std::fs::read(sequence.join("frame_000000.png"))
            .map(|bytes| bytes.starts_with(b"\x89PNG\r\n\x1a\n"))
            .unwrap_or(false);
        let generation = manifest_dir.join(scene);
        manifests &= generation.join("FMN_COMPLETE").is_file()
            && std::fs::read(generation.join("manifest.fmnp"))
                .ok()
                .and_then(|bytes| ProvenanceManifest::from_bytes(&bytes).ok())
                .is_some_and(|manifest| {
                    manifest.outputs.len() == 1
                        && (1..=10).all(|id| manifest.items.iter().any(|item| item.item_id == id))
                });
    }
    let lines: Vec<&str> = output.stdout.lines().collect();
    let ordered_records = lines.len() == 3
        && lines[0].contains("\"scene\":\"circle_shift.v1\"")
        && lines[1].contains("\"scene\":\"rectangle_shift.v1\"")
        && lines[2].contains("\"kind\":\"batch\"")
        && lines[2].contains("\"status\":\"ok\"")
        && lines[2].contains("\"succeeded\":2");
    ctx.event(
        LogEvent::new("e2e.cli.batch")
            .field("source", "builtin")
            .field("format", "png_sequence")
            .field("complete", truth(complete))
            .field("png_signatures", truth(signatures))
            .field("manifests", truth(manifests))
            .field("ordered_records", truth(ordered_records)),
    );
    ctx.counter("cli_batch_scenes", 2);
    ctx.counter("cli_batch_frames", frames as u64);
    if output.code != 0
        || !output.stderr.is_empty()
        || !complete
        || frames != 6
        || !signatures
        || !manifests
        || !ordered_records
    {
        return Err(fail(format!(
            "native CLI batch failed: code={} frames={frames} complete={complete} signatures={signatures} manifests={manifests} ordered={ordered_records} stdout={:?} stderr={:?}",
            output.code, output.stdout, output.stderr
        )));
    }
    Ok(RunOutcome::ok()
        .with_counter("cli_batch_scenes", 2)
        .with_counter("cli_batch_frames", 6))
}

/// Stock-CLI proof for the two native RGBA artifact adapters that share
/// Lumen's final conversion but have distinct publication semantics: PNG
/// captures exactly the completed scene state, while GIF streams the full
/// schedule without crossing the ffmpeg boundary.
fn cli_native_png_gif_run(ctx: &mut RunCtx) -> Result<RunOutcome, ScenarioError> {
    let dir = scenario_dir("cli_native_png_gif")?;
    let png_dir = dir.join("png");
    let gif_dir = dir.join("gif");
    std::fs::create_dir(&png_dir)
        .map_err(|error| fail(format!("create CLI PNG directory: {error}")))?;
    std::fs::create_dir(&gif_dir)
        .map_err(|error| fail(format!("create CLI GIF directory: {error}")))?;
    let png_dir_text = png_dir
        .to_str()
        .ok_or_else(|| fail("CLI PNG output path is not UTF-8"))?;
    let gif_dir_text = gif_dir
        .to_str()
        .ok_or_else(|| fail("CLI GIF output path is not UTF-8"))?;

    let render = |format: &str, output: &str| {
        fmn_cli::run([
            "--robot",
            "--format",
            format,
            "--resolution",
            "96x54",
            "--fps",
            "8",
            "--threads",
            "1",
            "--video_dir",
            output,
            fmn_cli::BUILTIN_SCENE_SOURCE,
            "circle_shift.v1",
        ])
    };
    let png = render("png", png_dir_text);
    let gif = render("gif", gif_dir_text);
    let png_signature = std::fs::read(png_dir.join("circle_shift.png"))
        .map(|bytes| bytes.starts_with(b"\x89PNG\r\n\x1a\n"))
        .unwrap_or(false);
    let gif_signature = std::fs::read(gif_dir.join("circle_shift.gif"))
        .map(|bytes| bytes.starts_with(b"GIF89a") && bytes.last() == Some(&b';'))
        .unwrap_or(false);
    let robot_records = png.stdout.lines().count() == 1
        && png.stdout.contains("\"format\":\"png\"")
        && png.stdout.contains("\"frames\":1")
        && gif.stdout.lines().count() == 1
        && gif.stdout.contains("\"format\":\"gif\"")
        && gif.stdout.contains("\"frames\":3");
    ctx.event(
        LogEvent::new("e2e.cli.native_rgba_artifacts")
            .field("png_signature", truth(png_signature))
            .field("gif_signature", truth(gif_signature))
            .field("robot_records", truth(robot_records)),
    );
    ctx.counter("cli_png_still_frames", u64::from(png_signature));
    ctx.counter(
        "cli_gif_stream_frames",
        if gif_signature && gif.stdout.contains("\"frames\":3") {
            3
        } else {
            0
        },
    );
    if png.code != 0
        || gif.code != 0
        || !png.stderr.is_empty()
        || !gif.stderr.is_empty()
        || !png_signature
        || !gif_signature
        || !robot_records
    {
        return Err(fail(format!(
            "native PNG/GIF CLI path failed: png=({},{:?},{:?}) gif=({},{:?},{:?})",
            png.code, png.stdout, png.stderr, gif.code, gif.stdout, gif.stderr
        )));
    }
    Ok(RunOutcome::ok()
        .with_counter("cli_png_still_frames", 1)
        .with_counter("cli_gif_stream_frames", 3))
}

/// A compiled FMTL/1 artifact crosses the stock CLI boundary, reconstructs
/// through fmn-scene's shared reader, renders through Lumen, and publishes
/// through Reel. This is the source-unedited native-artifact path rather than
/// another built-in registration.
fn cli_compiled_bundle_render_run(ctx: &mut RunCtx) -> Result<RunOutcome, ScenarioError> {
    let dir = scenario_dir("cli_compiled_bundle_render")?;
    let source = dir.join("compiled_wait.fmtl");
    let output_dir = dir.join("output");
    std::fs::create_dir(&output_dir)
        .map_err(|error| fail(format!("create compiled output directory: {error}")))?;
    let mut timeline =
        Timeline::new(8).map_err(|error| fail(format!("create compiled timeline: {error}")))?;
    timeline
        .wait(0.25)
        .map_err(|error| fail(format!("author compiled wait: {error}")))?;
    let bytes =
        fmn_scene::export_timeline_bundle(timeline, &mut Stage::new(), &RngRoot::from_seed(0))
            .map_err(|error| fail(format!("export compiled timeline: {error}")))?;
    std::fs::write(&source, bytes)
        .map_err(|error| fail(format!("write compiled artifact: {error}")))?;
    let output = fmn_cli::run([
        "--robot",
        "--format",
        "png_sequence",
        "--resolution",
        "96x54",
        "--threads",
        "1",
        "--video_dir",
        output_dir
            .to_str()
            .ok_or_else(|| fail("compiled output path is not UTF-8"))?,
        source
            .to_str()
            .ok_or_else(|| fail("compiled source path is not UTF-8"))?,
        "CompiledWait",
    ]);
    let sequence = output_dir.join("CompiledWait");
    let complete = sequence.join("FMN_COMPLETE").is_file();
    let pngs = std::fs::read_dir(&sequence)
        .map_err(|error| fail(format!("list compiled CLI PNG sequence: {error}")))?
        .filter_map(Result::ok)
        .filter(|entry| entry.path().extension().is_some_and(|ext| ext == "png"))
        .count();
    let signature = std::fs::read(sequence.join("frame_000000.png"))
        .map(|bytes| bytes.starts_with(b"\x89PNG\r\n\x1a\n"))
        .unwrap_or(false);
    let robot_record = output.stdout.lines().count() == 1
        && output.stdout.contains("\"kind\":\"render\"")
        && output.stdout.contains("\"source\":\"compiled\"")
        && output.stdout.contains("\"source_artifact\":")
        && output.stdout.contains("\"frames\":2");
    ctx.event(
        LogEvent::new("e2e.cli.render")
            .field("source", "compiled")
            .field("format", "png_sequence")
            .field("complete", truth(complete))
            .field("png_signature", truth(signature))
            .field("robot_record", truth(robot_record)),
    );
    ctx.counter("cli_compiled_render_frames", pngs as u64);
    ctx.counter("cli_compiled_png_signature", u64::from(signature));
    if output.code != 0
        || !output.stderr.is_empty()
        || !complete
        || pngs != 2
        || !signature
        || !robot_record
    {
        return Err(fail(format!(
            "compiled CLI render failed: code={} pngs={pngs} complete={complete} signature={signature} stdout={:?} stderr={:?}",
            output.code, output.stdout, output.stderr
        )));
    }
    Ok(RunOutcome::ok()
        .with_counter("cli_compiled_render_frames", 2)
        .with_counter("cli_compiled_png_signature", 1))
}

// ---------------------------------------------------------------------------
// Lifecycle drill
// ---------------------------------------------------------------------------

/// The certified single-threaded frame of a stage, encoded into its
/// canonical document — the same renderer `tests/scene_goldens.rs`
/// injects into the corpus artifact form.
fn render_certified_doc(stage: &Stage) -> Vec<u8> {
    let config = scene_goldens::frame_config();
    let mut plan = RenderPlan::new();
    plan.sync(stage, 0)
        .expect("valid certified document fixture");
    let mono =
        MonoTable::build(&plan, config.map).expect("bounded certified document monotone table");
    let mut binning = Binning::build(&plan, config.viewport, TILING, config.map)
        .expect("bounded conformance binning");
    binning
        .prune_occluded(&plan)
        .expect("occlusion pruning matches the plan");
    let frame =
        FrameJob::with_identity(&plan, &mono, &binning, config, EngineIdentity::certified())
            .expect("frame job matches the plan")
            .render(1)
            .expect("the engine renders the frame");
    encode_frame(&frame).expect("the frame encodes into its canonical document")
}

/// Construct → snapshot → transform → snapshot: the scene_goldens
/// lifecycle form with geometry assertions (member/point counts move with
/// the lifecycle points, never silently empty).
fn lifecycle_construct_run(ctx: &mut RunCtx) -> Result<RunOutcome, ScenarioError> {
    let corpus = scene_goldens::corpus();
    let case = scene_goldens::scene_named("circle_tex_label.v1")
        .ok_or_else(|| fail("circle_tex_label.v1 is missing from the corpus"))?;
    let built = (case.build)(corpus);

    let count_geometry = |stage: &Stage| -> (u64, u64) {
        let mut members = 0_u64;
        let mut points = 0_u64;
        for &root in &built.roots {
            for member in stage.family(root) {
                members += 1;
                points += stage.get_points(member).unwrap_or_default().len() as u64;
            }
        }
        (members, points)
    };

    let (members_pre, points_pre) = count_geometry(&built.stage);
    let mut stage = built.stage;
    scene_goldens::apply_lifecycle_transform(&mut stage, &built.roots, 0);
    let (members_post, points_post) = count_geometry(&stage);

    ctx.event(
        LogEvent::new(e2e::spans::SCENE_CONSTRUCT)
            .field("scene", case.name)
            .field("members", members_pre)
            .field("points", points_pre),
    );
    ctx.event(
        LogEvent::new("e2e.lifecycle.transform")
            .field("members", members_post)
            .field("points", points_post),
    );
    ctx.counter("lifecycle_members", members_pre);
    ctx.counter("lifecycle_points_pre", points_pre);
    ctx.counter("lifecycle_points_post", points_post);

    if members_pre == 0 || points_pre == 0 || points_post == 0 {
        return Err(fail(
            "lifecycle geometry went empty — the snapshot would lock nothing",
        ));
    }

    // The locked artifact is the corpus lifecycle form: geometry
    // snapshots plus certified frames at both lifecycle points.
    let bytes = scene_goldens::artifact(case, corpus, 0, &render_certified_doc);
    Ok(RunOutcome::ok()
        .with_artifact("lifecycle.circle_tex_label.construct_transform.v1", bytes)
        .with_counter("lifecycle_members", members_pre)
        .with_counter("lifecycle_points_pre", points_pre)
        .with_counter("lifecycle_points_post", points_post))
}

/// Journal round-trip: the scene's command record (add, pure play,
/// stateful wait) serializes, deserializes, hashes identically, and
/// `plan_replay` reuses exactly the non-barrier prefix — and stops with
/// the named reason when the incoming stream diverges.
fn lifecycle_journal_run(ctx: &mut RunCtx) -> Result<RunOutcome, ScenarioError> {
    let mut journal = Journal::new();
    let commands = [
        (CommandKind::Add, EffectClass::Pure, "add circle+label"),
        (CommandKind::Play, EffectClass::Pure, "play shift(circle)"),
        (
            CommandKind::Wait,
            EffectClass::Stateful(vec![fmn_scene::journal::ImpureEffectTag::StopCondition]),
            "wait(0.125)",
        ),
    ];
    let mut records = Vec::new();
    for (index, (kind, effect, label)) in commands.iter().enumerate() {
        let identity = sha256(label.as_bytes());
        let state_hash = sha256(format!("e2e-state-{index}").as_bytes());
        let record = CommandRecord {
            kind: *kind,
            identity,
            label: (*label).to_string(),
        };
        let entry = Entry {
            command: record.clone(),
            effect: effect.clone(),
            reads: Vec::new(),
            subprocesses: Vec::new(),
            checkpoint: (index == 0).then(|| format!("checkpoint-{index}").into_bytes()),
            state_hash,
        };
        journal
            .record(entry.clone())
            .map_err(|error| fail(format!("journal records: {error}")))?;
        // The same entries ride the run's repro bundle (the §18 closure).
        ctx.record_journal(entry);
        records.push(record);
    }

    let bytes = journal
        .to_bytes()
        .map_err(|error| fail(format!("journal encodes: {error}")))?;
    let restored =
        Journal::from_bytes(&bytes).map_err(|error| fail(format!("journal decodes: {error}")))?;
    let hash_before = journal
        .content_hash()
        .map_err(|error| fail(format!("journal hashes: {error}")))?;
    let hash_after = restored
        .content_hash()
        .map_err(|error| fail(format!("restored journal hashes: {error}")))?;
    let roundtrip_equal = hash_before == hash_after && restored.entries().len() == records.len();

    // Full replay: the incoming stream matches the record.
    let plan = plan_replay(&restored, &records, &|_read| true);
    let full_reuse = plan.reuse == records.len() && plan.reason.is_none();
    let resumes_at_checkpoint = plan.resume_checkpoint == Some(0);

    // Diverged replay: a different third command stops the walk with the
    // named reason at exactly that index.
    let mut divergent = records.clone();
    divergent[2] = CommandRecord {
        kind: CommandKind::Wait,
        identity: sha256(b"wait(0.5)"),
        label: "wait(0.5)".to_string(),
    };
    let divergent_plan = plan_replay(&restored, &divergent, &|_read| true);
    let mismatch_detected = divergent_plan.reuse == 2
        && matches!(
            divergent_plan.reason,
            Some(fmn_scene::journal::InvalidationReason::CommandMismatch { index: 2 })
        );

    ctx.event(
        LogEvent::new("e2e.lifecycle.journal")
            .field("entries", records.len() as u64)
            .field("roundtrip_equal", truth(roundtrip_equal))
            .field("replay_reuse", plan.reuse as u64)
            .field("mismatch_detected", truth(mismatch_detected)),
    );
    ctx.counter("journal_roundtrip_equal", u64::from(roundtrip_equal));
    ctx.counter("journal_replay_reuse", plan.reuse as u64);
    ctx.counter("journal_mismatch_detected", u64::from(mismatch_detected));

    if !(roundtrip_equal && full_reuse && resumes_at_checkpoint && mismatch_detected) {
        return Err(fail(format!(
            "journal round-trip broke: equal={roundtrip_equal} reuse={} checkpoint={:?} \
             mismatch={mismatch_detected}",
            plan.reuse, plan.resume_checkpoint
        )));
    }
    Ok(RunOutcome::ok()
        .with_counter("journal_roundtrip_equal", 1)
        .with_counter("journal_replay_reuse", plan.reuse as u64)
        .with_counter("journal_mismatch_detected", 1))
}

/// Bounded E2E registration for PG-5's user-visible definition surface.
///
/// The release-perf producer remains the authoritative complete-corpus proof
/// for direct, frame-parallel, and ordered-pipeline scheduling. This fast
/// scenario binds that definition to one real committed scene rendered at the
/// permanent per-commit direct thread counts, so a CLI/definition landing
/// cannot silently lose its executable certified-render anchor.
fn pg5_direct_schedule_run(ctx: &mut RunCtx) -> Result<RunOutcome, ScenarioError> {
    let definition =
        Pg5Definition::new().map_err(|error| fail(format!("PG-5 definition identity: {error}")))?;
    definition
        .validate_corpus_lock()
        .map_err(|error| fail(format!("PG-5 corpus identity: {error}")))?;
    let case = scene_goldens::scene_named("circle_tex_label.v1")
        .ok_or_else(|| fail("PG-5 registered scene is missing"))?;
    let built = (case.build)(scene_goldens::corpus());
    let config = scene_goldens::frame_config();
    let mut plan = RenderPlan::new();
    plan.sync(&built.stage, 0)
        .map_err(|error| fail(format!("{} render plan: {error}", case.name)))?;
    let mono = MonoTable::build(&plan, config.map)
        .map_err(|error| fail(format!("PG-5 monotone table: {error}")))?;
    let mut binning = Binning::build(&plan, config.viewport, TILING, config.map)
        .expect("bounded conformance binning");
    binning
        .prune_occluded(&plan)
        .map_err(|error| fail(format!("PG-5 binning: {error}")))?;

    let mut digests = Vec::with_capacity(PG5_DIRECT_THREADS.len());
    for threads in PG5_DIRECT_THREADS {
        let job = FrameJob::with_identity(&plan, &mono, &binning, config, pg5_identity())
            .map_err(|error| fail(format!("PG-5 {threads}-thread job: {error}")))?;
        let frame = job
            .render(threads)
            .map_err(|error| fail(format!("PG-5 {threads}-thread render: {error}")))?;
        digests.push(
            frame_digest(&frame)
                .map_err(|error| fail(format!("PG-5 {threads}-thread digest: {error}")))?,
        );
    }
    let reference = digests
        .first()
        .copied()
        .ok_or_else(|| fail("PG-5 direct thread profile is empty"))?;
    let direct_mismatches = u64::try_from(
        digests
            .iter()
            .skip(1)
            // ubs:ignore — public frame self-goldens, not authentication material.
            .filter(|&&digest| digest != reference)
            .count(),
    )
    .map_err(|_| fail("PG-5 mismatch count exceeds u64"))?;

    ctx.record_asset(
        "pg5.certified-thread-matrix.definition",
        definition.to_tsv().as_bytes(),
    );
    ctx.event(
        LogEvent::new("performance.pg5.direct_schedule")
            .field("scene", case.name)
            .field("direct_threads", "1,4,16")
            .field("direct_mismatches", direct_mismatches)
            .field("reference_digest", reference.to_string())
            .field("benchmark_definition", definition.digest().to_string()),
    );
    ctx.counter("pg5_direct_threads", PG5_DIRECT_THREADS.len() as u64);
    ctx.counter("pg5_direct_mismatches", direct_mismatches);
    if direct_mismatches != 0 {
        return Err(fail(format!(
            "PG-5 representative certified render drifted at {direct_mismatches} thread counts"
        )));
    }
    Ok(RunOutcome::ok()
        .with_counter("pg5_direct_threads", PG5_DIRECT_THREADS.len() as u64)
        .with_counter("pg5_direct_mismatches", 0))
}

/// PG-6's user-visible evidence surface, registered end to end: a committed
/// corpus scene passes through the same warm/reuse arena lifecycle as the
/// release-perf producer, and the scenario log retains the observable proof.
fn pg6_allocation_reuse_run(ctx: &mut RunCtx) -> Result<RunOutcome, ScenarioError> {
    let definition = Pg6Definition::new();
    definition
        .validate_corpus_lock()
        .map_err(|error| fail(format!("PG-6 corpus identity: {error}")))?;
    let case = scene_goldens::scene_named("circle_tex_label.v1")
        .ok_or_else(|| fail("PG-6 registered scene is missing"))?;
    let built = (case.build)(scene_goldens::corpus());
    let config = scene_goldens::frame_config();
    let mut plan = RenderPlan::new();
    plan.sync(&built.stage, 0)
        .map_err(|error| fail(format!("{} render plan: {error}", case.name)))?;
    let mono = MonoTable::build(&plan, config.map)
        .map_err(|error| fail(format!("PG-6 monotone table: {error}")))?;
    let mut binning = Binning::build(&plan, config.viewport, TILING, config.map)
        .expect("bounded conformance binning");
    binning
        .prune_occluded(&plan)
        .map_err(|error| fail(format!("PG-6 binning: {error}")))?;

    let mut arena = FrameArena::new();
    let (mut frame, warm_digest, warm_allocations) = {
        let job =
            FrameJob::with_identity_in(&mut arena, &plan, &mono, &binning, config, pg6_identity())
                .map_err(|error| fail(format!("PG-6 warm job: {error}")))?;
        let frame = job
            .render(PG6_THREADS)
            .map_err(|error| fail(format!("PG-6 warm render: {error}")))?;
        let digest =
            frame_digest(&frame).map_err(|error| fail(format!("PG-6 warm digest: {error}")))?;
        (frame, digest, job.allocation_stats().heap_allocs_this_frame)
    };
    let (measured_digest, measured) = {
        let job =
            FrameJob::with_identity_in(&mut arena, &plan, &mono, &binning, config, pg6_identity())
                .map_err(|error| fail(format!("PG-6 measured job: {error}")))?;
        job.render_into(PG6_THREADS, &mut frame)
            .map_err(|error| fail(format!("PG-6 measured render: {error}")))?;
        let digest =
            frame_digest(&frame).map_err(|error| fail(format!("PG-6 measured digest: {error}")))?;
        (digest, job.allocation_stats())
    };
    // ubs:ignore — public deterministic artifact identity, not authentication material.
    let digest_equal = warm_digest == measured_digest;
    let allocation_free = measured.heap_allocs_this_frame == 0;
    ctx.record_asset(
        "pg6.primitive-steady-allocations.definition",
        definition.to_tsv().as_bytes(),
    );
    ctx.event(
        LogEvent::new("performance.pg6.allocations")
            .field("scene", case.name)
            .field("warm_digest", warm_digest.to_string())
            .field("measured_digest", measured_digest.to_string())
            .field("warm_allocations", warm_allocations)
            .field("measured_allocations", measured.heap_allocs_this_frame)
            .field("arena_bytes", measured.arena_buffer_bytes)
            .field("pool_slots", measured.pool_slots),
    );
    ctx.counter("pg6_digest_equal", u64::from(digest_equal));
    ctx.counter("pg6_measured_allocations", measured.heap_allocs_this_frame);
    if !digest_equal || !allocation_free {
        return Err(fail(format!(
            "PG-6 arena reuse drifted: digest_equal={digest_equal}, measured_allocations={}",
            measured.heap_allocs_this_frame
        )));
    }
    Ok(RunOutcome::ok()
        .with_counter("pg6_digest_equal", 1)
        .with_counter("pg6_measured_allocations", 0))
}

/// PG-6's scheduled full-tier corpus proof. This is the same three-frame UHD
/// production function the peak-RSS sampler runs during every repetition; the
/// e2e row validates real rendering and lock identity, not a reduced proxy.
fn pg6_peak_gallery_run(ctx: &mut RunCtx) -> Result<RunOutcome, ScenarioError> {
    let definition = Pg6PeakDefinition::new();
    definition
        .validate_corpus_lock()
        .map_err(|error| fail(format!("PG-6 peak corpus identity: {error}")))?;
    let cases = render_locked_gallery_once()
        .map_err(|error| fail(format!("PG-6 peak gallery render: {error}")))?;
    let complete = cases.len() == PG6_PEAK_CASE_COUNT;
    let distinct = complete
        && cases[0].frame_digest != cases[1].frame_digest
        && cases[0].frame_digest != cases[2].frame_digest
        && cases[1].frame_digest != cases[2].frame_digest;
    let preparation_bytes = cases
        .iter()
        .map(|case| case.preparation_bytes)
        .max()
        .unwrap_or(0);

    ctx.record_asset(
        "pg6.gallery-4k-3d-peak.definition",
        definition.to_tsv().as_bytes(),
    );
    ctx.event(
        LogEvent::new("performance.pg6.peak_gallery")
            .field("frames", cases.len())
            .field("width", u64::from(PG6_PEAK_WIDTH))
            .field("height", u64::from(PG6_PEAK_HEIGHT))
            .field("distinct_digests", distinct)
            .field("max_preparation_bytes", preparation_bytes)
            .field("benchmark_definition", definition.digest().to_string()),
    );
    ctx.counter("pg6_peak_frames", cases.len() as u64);
    ctx.counter("pg6_peak_distinct_digests", u64::from(distinct));
    if !complete || !distinct || preparation_bytes == 0 {
        return Err(fail(format!(
            "PG-6 peak gallery proof incomplete: frames={}, distinct={distinct}, max_preparation_bytes={preparation_bytes}",
            cases.len(),
        )));
    }
    Ok(RunOutcome::ok()
        .with_counter("pg6_peak_frames", PG6_PEAK_CASE_COUNT as u64)
        .with_counter("pg6_peak_distinct_digests", 1))
}

// ---------------------------------------------------------------------------
// Log-machinery scenarios
// ---------------------------------------------------------------------------

/// Preflight: a Tex-bearing scene's constructed-scene preflight typesets
/// every registered string **before frame zero** — the assertion the
/// harness exists to make, because nothing in the pixels shows it.
fn logs_preflight_run(ctx: &mut RunCtx) -> Result<RunOutcome, ScenarioError> {
    ctx.set_fps((FPS, 1));
    // The real event order, recorded in-run: (ordinal, kind, payload).
    #[derive(Clone)]
    enum Trace {
        Preflight { typesets: u64 },
        FirstCapture { frame: i64 },
    }
    let trace: Arc<Mutex<Vec<Trace>>> = Arc::new(Mutex::new(Vec::new()));

    let sources: [&'static str; 2] = [r"\odot", r"\frac{1}{2}"];
    let label = tex_label(sources[0], 40.0)?;

    let mut scene = Scene::new(
        RuntimeConfig {
            fps: FPS,
            ..RuntimeConfig::default()
        },
        ctx.seed,
    )
    .map_err(|error| fail(format!("scene constructs: {error}")))?;

    let hook_trace = Arc::clone(&trace);
    scene
        .set_preflight_hook(move |_stage, _anchors| {
            let mut typesets = 0_u64;
            for source in sources {
                scene_goldens::corpus()
                    .tex
                    .typeset(fmn_tex::Mode::Math(fmd_math::Style::Text), source)
                    .map_err(|error| {
                        IntegrationError::new("preflight", format!("typeset {source:?}: {error}"))
                    })?;
                typesets += 1;
            }
            hook_trace
                .lock()
                .map_err(|_| IntegrationError::new("preflight", "trace lock poisoned"))?
                .push(Trace::Preflight { typesets });
            Ok(())
        })
        .map_err(|error| fail(format!("preflight hook installs: {error}")))?;

    struct TraceSink {
        trace: Arc<Mutex<Vec<Trace>>>,
    }
    impl SceneSink for TraceSink {
        fn capture(
            &mut self,
            _reason: CaptureReason,
            packet: FramePacket,
        ) -> Result<(), IntegrationError> {
            let mut guard = self
                .trace
                .lock()
                .map_err(|_| IntegrationError::new("sink", "trace lock poisoned"))?;
            if !guard
                .iter()
                .any(|event| matches!(event, Trace::FirstCapture { .. }))
            {
                guard.push(Trace::FirstCapture {
                    frame: packet.frame_index(),
                });
            }
            Ok(())
        }
    }

    let mut program = MatrixProgram {
        label: Some(label),
        wait_only: false,
    };
    let mut sink = TraceSink {
        trace: Arc::clone(&trace),
    };
    scene
        .run(&mut program, &mut sink)
        .map_err(|error| fail(format!("scene runs: {error}")))?;

    let events = trace
        .lock()
        .map_err(|_| fail("trace lock poisoned"))?
        .clone();
    let mut typesets = 0_u64;
    let mut preflight_ordinal = None;
    let mut first_capture_ordinal = None;
    let mut first_frame = 0_i64;
    for (ordinal, event) in events.iter().enumerate() {
        match event {
            Trace::Preflight { typesets: n } => {
                typesets = *n;
                preflight_ordinal = Some(ordinal);
            }
            Trace::FirstCapture { frame } => {
                first_capture_ordinal = Some(ordinal);
                first_frame = *frame;
            }
        }
    }
    // The contract (§17.1): the constructed-scene preflight — and with it
    // every typeset — completes before the first frame is emitted. (The
    // first *emitted* frame of an animated play is index 1: frame 0 is the
    // segment's begin state, never re-emitted.)
    let ordered = matches!(
        (preflight_ordinal, first_capture_ordinal),
        (Some(before), Some(after)) if before < after
    );

    ctx.event(
        LogEvent::new(e2e::spans::PREFLIGHT)
            .field("typesets", typesets)
            .field("before_first_frame", truth(ordered)),
    );
    ctx.event(
        LogEvent::new(e2e::spans::RENDER_FRAME)
            .field("frame", first_frame.max(0) as u64)
            .field("first_capture", "true"),
    );
    ctx.counter(e2e::counters::TYPESETS, typesets);

    if !ordered || typesets != 2 {
        return Err(fail(format!(
            "preflight contract broke: typesets={typesets} ordered={ordered} \
             first_frame={first_frame}"
        )));
    }
    Ok(RunOutcome::ok()
        .with_counter("typesets", typesets)
        .with_counter("preflight_before_first_frame", 1))
}

/// Purity: the segment classification is recorded for both classes — the
/// play (pure: frame-parallel eligible) and the wait (stateful: serial
/// front-end with its recorded reason).
fn logs_purity_run(ctx: &mut RunCtx) -> Result<RunOutcome, ScenarioError> {
    ctx.set_fps((FPS, 1));
    let label = tex_label(r"\odot", 40.0)?;
    let reports: Arc<Mutex<Vec<(String, String, i64)>>> = Arc::new(Mutex::new(Vec::new()));

    struct PurityProgram {
        label: Option<VMobject>,
        reports: Arc<Mutex<Vec<(String, String, i64)>>>,
    }
    impl SceneProgram for PurityProgram {
        fn name(&self) -> &str {
            "e2e_purity"
        }
        fn construct(
            &mut self,
            scene: &mut Scene,
            sink: &mut dyn SceneSink,
        ) -> Result<(), SceneError> {
            let circle = scene.add_mobject(Circle::new().radius(0.9))?;
            scene
                .stage_mut()
                .set_fill(circle, Some(BLUE_C), Some(0.35), Some(0.0), true);
            let label = self.label.take().ok_or(SceneError::InvalidLifecycle(
                "purity program constructed twice",
            ))?;
            scene.add_mobject(label)?;
            let builder = circle
                .animate()
                .set_anim_args(AnimateArgs {
                    run_time: Some(0.25),
                    rate_func: Some(fmn_core::rate::linear),
                    ..AnimateArgs::default()
                })
                .map_err(|error| SceneError::Animation(error.into()))?
                .shift([0.6, 0.2, 0.0])
                .map_err(|error| SceneError::Animation(error.into()))?;
            let animation = fmn_anim::prepare_animation(builder, scene.stage_mut())?;
            if let Some(report) = scene.play(vec![animation], PlayOverrides::default(), sink)? {
                let purity = if report.purity.is_pure() {
                    "pure"
                } else {
                    "stateful"
                };
                self.reports
                    .lock()
                    .map_err(|_| SceneError::InvalidLifecycle("report lock poisoned"))?
                    .push(("play".to_string(), purity.to_string(), report.n_frames));
            }
            let report = scene.wait_until(0.125, &mut |_stage| false, sink)?;
            let purity = if report.purity.is_pure() {
                "pure"
            } else {
                "stateful"
            };
            self.reports
                .lock()
                .map_err(|_| SceneError::InvalidLifecycle("report lock poisoned"))?
                .push(("wait".to_string(), purity.to_string(), report.n_frames));
            Ok(())
        }
    }

    let mut scene = Scene::new(
        RuntimeConfig {
            fps: FPS,
            ..RuntimeConfig::default()
        },
        ctx.seed,
    )
    .map_err(|error| fail(format!("scene constructs: {error}")))?;
    let mut program = PurityProgram {
        label: Some(label),
        reports: Arc::clone(&reports),
    };
    let mut sink = PacketSink::default();
    scene
        .run(&mut program, &mut sink)
        .map_err(|error| fail(format!("scene runs: {error}")))?;

    let reports = reports
        .lock()
        .map_err(|_| fail("report lock poisoned"))?
        .clone();
    let mut saw_pure_play = false;
    let mut saw_stateful_wait = false;
    for (kind, purity, frames) in &reports {
        ctx.event(
            LogEvent::new(e2e::spans::PURITY)
                .field("segment", kind.as_str())
                .field("purity", purity.as_str())
                .field("frames", (*frames).max(0) as u64),
        );
        saw_pure_play |= kind == "play" && purity == "pure";
        saw_stateful_wait |= kind == "wait" && purity == "stateful";
    }
    ctx.counter("segments_classified", reports.len() as u64);
    if !(saw_pure_play && saw_stateful_wait) {
        return Err(fail(format!("purity classifications drifted: {reports:?}")));
    }
    Ok(RunOutcome::ok()
        .with_counter("segments_classified", reports.len() as u64)
        .with_counter("pure_segments", 1))
}

/// Engine identity and plan decision on a reproducible-class run: the
/// certified identity is recorded with its tuning provenance pinned.
fn logs_engine_identity_run(ctx: &mut RunCtx) -> Result<RunOutcome, ScenarioError> {
    ctx.set_fps((FPS, 1));
    let preset = PRESETS[0];
    let topology = HardwareTopology::fallback(8);
    let plan = ExecutionPlan::derive(
        PlanRequest::certified(
            RenderIntent::Offline,
            SurfaceSpec::lumen(preset.width, preset.height),
            OutputPixelFormat::Rgba8,
        ),
        &topology,
        None,
    )
    .map_err(|error| fail(format!("certified plan derives: {error}")))?;
    ctx.event(
        LogEvent::new(e2e::spans::EXECUTION_PLAN)
            .field("determinism", "certified")
            .field("tuning_source", tuning_source_name(plan.tuning_source))
            .field("fine_tile", u64::from(plan.fine_tile))
            .field("macro_tile", u64::from(plan.macro_tile)),
    );

    let identity = EngineIdentity::certified();
    ctx.event(
        LogEvent::new(e2e::spans::ENGINE)
            .field("engine", engine_kind_name(identity.engine))
            .field("reproducible", "true")
            .field("renderer_version", u64::from(identity.renderer_version)),
    );

    // Render one certified frame so the identity event is attached to real
    // engine work, not a bare declaration.
    let packets = run_matrix_scene(ctx.seed, true)?;
    let terminal = packets
        .last()
        .ok_or_else(|| fail("scene produced no terminal packet"))?;
    let frame = render_packet(terminal, identity, preset_frame_config(&preset), 1)?;
    let doc = encode_frame(&frame).map_err(|error| fail(format!("encode: {error}")))?;
    ctx.event(
        LogEvent::new(e2e::spans::RENDER_FRAME)
            .field("frame", 0_u64)
            .field("engine", engine_kind_name(identity.engine)),
    );
    ctx.counter("engine_certified_frames", 1);
    Ok(RunOutcome::ok()
        .with_counter("engine_certified_frames", 1)
        .with_counter("certified_doc_bytes", doc.len() as u64))
}

/// §15.1's product-facing proof: compile and execute a primitive scene using
/// only the `fmn` facade. The facade delegates to the same Scene runtime that
/// the rest of this catalog exercises; the capture count is the observable
/// proof that the public entry point crossed the real frame boundary.
fn public_facade_run(ctx: &mut RunCtx) -> Result<RunOutcome, ScenarioError> {
    use fmn::prelude::*;

    #[derive(Default)]
    struct FacadeProgram;

    impl SceneConstruct for FacadeProgram {
        fn name(&self) -> &str {
            "public_facade_circle_shift"
        }

        fn construct(&mut self, stage: &mut fmn::Stage<'_>) -> fmn::Result<()> {
            let circle = stage.add(Circle::new().radius(0.75).color(BLUE_C))?;
            let square = stage.add(Square::new().side_length(1.25).color(YELLOW))?;
            let movement = circle
                .animate()
                .set_anim_args(AnimateArgs {
                    run_time: Some(0.25),
                    rate_func: Some(fmn::core::rate::linear),
                    ..AnimateArgs::default()
                })?
                .shift([0.5, 0.25, 0.0])?;
            let square_movement = square
                .animate()
                .set_anim_args(AnimateArgs {
                    run_time: Some(0.25),
                    rate_func: Some(fmn::core::rate::linear),
                    ..AnimateArgs::default()
                })?
                .shift([-0.25, 0.0, 0.0])?
                .rotate(PI / 4.0)?
                .set_opacity(0.5)?;
            stage.play((movement, square_movement))?;
            stage.wait(0.125)?;
            Ok(())
        }
    }

    #[derive(Default)]
    struct FacadeSink {
        captures: u64,
    }

    impl SceneSink for FacadeSink {
        fn capture(
            &mut self,
            _reason: CaptureReason,
            _packet: fmn::animation::FramePacket,
        ) -> std::result::Result<(), IntegrationError> {
            self.captures += 1;
            Ok(())
        }
    }

    let mut sink = FacadeSink::default();
    let completed = run_scene(
        &mut FacadeProgram,
        RuntimeConfig {
            fps: FPS,
            ..RuntimeConfig::default()
        },
        0xFACADE,
        &mut sink,
    )
    .map_err(|error| fail(format!("public facade: {error}")))?;
    if completed.report().play_count != 2 || sink.captures != 3 {
        return Err(fail(format!(
            "public facade produced {} plays and {} captures, expected 2 and 3",
            completed.report().play_count,
            sink.captures
        )));
    }

    ctx.event(
        LogEvent::new(e2e::spans::SCENE_CONSTRUCT)
            .field("scene", "public_facade_circle_shift")
            .field("captures", sink.captures),
    );
    ctx.counter("public_facade_captures", sink.captures);
    Ok(RunOutcome::ok()
        .with_counter("public_facade_plays", completed.report().play_count)
        .with_counter("public_facade_captures", sink.captures))
}

// ---------------------------------------------------------------------------
// The catalog
// ---------------------------------------------------------------------------

/// A fast-tier scenario with the NDJSON schema assertion folded in:
/// every scenario's captured log validates against the harness's own
/// schema on every run, not just in a dedicated drill. Chain `.tier(...)`
/// and `.regression(...)` at the call site for full-tier rows and drills.
fn spec(
    name: &'static str,
    class: ScenarioClass,
    surface: Surface,
    invocation: Invocation,
    mut assertions: Vec<Assertion>,
    logs: Vec<LogExpect>,
) -> ScenarioSpec {
    assertions.push(Assertion::NdjsonSchema);
    ScenarioSpec::new(name, class, surface, invocation)
        .assertions(assertions)
        .logs(logs)
}

/// The seed scenario catalog: every registered e2e scenario, as data.
#[must_use]
pub fn catalog() -> Vec<ScenarioSpec> {
    let mut specs = Vec::new();

    // RENDER-MATRIX: every preset × native sink × engine class.
    for preset in PRESETS {
        for sink_kind in [SinkKind::Y4m, SinkKind::PngSequence] {
            for certified in [true, false] {
                specs.push(matrix_spec(preset, sink_kind, certified, None));
            }
        }
    }
    specs.push(spec(
        "render_matrix.cli_builtin_png_sequence.v1",
        ScenarioClass::RenderMatrix,
        Surface::CliInProcess,
        Invocation::new(cli_builtin_render_run),
        vec![
            Assertion::ExitCode(0),
            counter_eq("cli_render_frames", 3),
            counter_eq("cli_png_signature", 1),
            counter_eq("cli_manifest", 1),
        ],
        vec![LogExpect::span_present(
            "e2e.cli.render",
            vec![
                FieldPred::str_eq("source", "builtin"),
                FieldPred::str_eq("format", "png_sequence"),
                FieldPred::str_eq("complete", "true"),
                FieldPred::str_eq("png_signature", "true"),
                FieldPred::str_eq("manifest_complete", "true"),
                FieldPred::str_eq("manifest_valid", "true"),
                FieldPred::str_eq("robot_record", "true"),
            ],
        )],
    ));
    specs.push(spec(
        "render_matrix.studio_builtin_preview.v1",
        ScenarioClass::RenderMatrix,
        Surface::StudioInProcess,
        Invocation::new(studio_preview_run),
        vec![
            Assertion::ExitCode(0),
            Assertion::FileInventory(vec!["studio_frame.png".to_owned()]),
            counter_eq("studio_preview_frames", 1),
            counter_eq("studio_png_dimensions", 1),
            counter_eq("studio_backend_journaled", 1),
        ],
        vec![LogExpect::span_present(
            "e2e.studio.preview",
            vec![
                FieldPred::str_eq("scene", "circle_shift.v1"),
                FieldPred::u64_eq("frame", 1),
                FieldPred::str_eq("encoding", "png"),
                FieldPred::str_eq("backend_is_stream", "true"),
                FieldPred::str_eq("dimensions_match", "true"),
            ],
        )],
    ));
    specs.push(spec(
        "render_matrix.cli_prerun_subdivide_y4m.v1",
        ScenarioClass::RenderMatrix,
        Surface::CliInProcess,
        Invocation::new(cli_prerun_subdivide_run),
        vec![
            Assertion::ExitCode(0),
            counter_eq("cli_prerun_frames", 3),
            counter_eq("cli_subdivision_segments", 2),
            counter_eq("cli_subdivision_frames", 3),
        ],
        vec![LogExpect::span_present(
            "e2e.cli.prerun_subdivide",
            vec![
                FieldPred::str_eq("prerun_record", "true"),
                FieldPred::str_eq("render_records", "true"),
                FieldPred::str_eq("complete", "true"),
                FieldPred::u64_eq("rendered_frames", 3),
            ],
        )],
    ));
    specs.push(spec(
        "render_matrix.cli_batch_png_sequence.v1",
        ScenarioClass::RenderMatrix,
        Surface::CliInProcess,
        Invocation::new(cli_batch_render_run),
        vec![
            Assertion::ExitCode(0),
            counter_eq("cli_batch_scenes", 2),
            counter_eq("cli_batch_frames", 6),
        ],
        vec![LogExpect::span_present(
            "e2e.cli.batch",
            vec![
                FieldPred::str_eq("source", "builtin"),
                FieldPred::str_eq("format", "png_sequence"),
                FieldPred::str_eq("complete", "true"),
                FieldPred::str_eq("png_signatures", "true"),
                FieldPred::str_eq("manifests", "true"),
                FieldPred::str_eq("ordered_records", "true"),
            ],
        )],
    ));
    specs.push(spec(
        "render_matrix.cli_native_png_gif.v1",
        ScenarioClass::RenderMatrix,
        Surface::CliInProcess,
        Invocation::new(cli_native_png_gif_run),
        vec![
            Assertion::ExitCode(0),
            counter_eq("cli_png_still_frames", 1),
            counter_eq("cli_gif_stream_frames", 3),
        ],
        vec![LogExpect::span_present(
            "e2e.cli.native_rgba_artifacts",
            vec![
                FieldPred::str_eq("png_signature", "true"),
                FieldPred::str_eq("gif_signature", "true"),
                FieldPred::str_eq("robot_records", "true"),
            ],
        )],
    ));
    specs.push(spec(
        "render_matrix.cli_compiled_fmtl_png_sequence.v1",
        ScenarioClass::RenderMatrix,
        Surface::CliInProcess,
        Invocation::new(cli_compiled_bundle_render_run),
        vec![
            Assertion::ExitCode(0),
            counter_eq("cli_compiled_render_frames", 2),
            counter_eq("cli_compiled_png_signature", 1),
        ],
        vec![LogExpect::span_present(
            "e2e.cli.render",
            vec![
                FieldPred::str_eq("source", "compiled"),
                FieldPred::str_eq("format", "png_sequence"),
                FieldPred::str_eq("complete", "true"),
                FieldPred::str_eq("png_signature", "true"),
                FieldPred::str_eq("robot_record", "true"),
            ],
        )],
    ));

    // DETERMINISM DRILLS.
    specs.push(spec(
        "determinism.repeat_byte_identical.v1",
        ScenarioClass::DeterminismDrill,
        Surface::RustApi,
        Invocation::new(determinism_repeat_run),
        vec![
            Assertion::ExitCode(0),
            counter_eq("repeat_byte_equal", 1),
            Assertion::GoldenLock {
                suite: E2E_SUITE,
                scope: Scope::Certified,
            },
        ],
        vec![
            LogExpect::counter_ge("repeat_byte_equal", 1),
            LogExpect::no_event("e2e.failure"),
        ],
    ));
    specs.push(spec(
        "determinism.threads_1_4_byte_identical.v1",
        ScenarioClass::DeterminismDrill,
        Surface::RustApi,
        Invocation::new(|ctx| determinism_threads_run(ctx, 4)),
        vec![Assertion::ExitCode(0), counter_eq("thread_byte_equal", 1)],
        vec![LogExpect::counter_ge("thread_byte_equal", 1)],
    ));
    specs.push(
        spec(
            "determinism.threads_16_byte_identical.v1",
            ScenarioClass::DeterminismDrill,
            Surface::RustApi,
            Invocation::new(|ctx| determinism_threads_run(ctx, 16)),
            vec![Assertion::ExitCode(0), counter_eq("thread_byte_equal", 1)],
            vec![LogExpect::counter_ge("thread_byte_equal", 1)],
        )
        .tier(Tier::Full),
    );

    // FAILURE-PATH DRILLS.
    specs.push(spec(
        "failure_path.tex_unsupported_construct_named.v1",
        ScenarioClass::FailurePath,
        Surface::RustApi,
        Invocation::new(failure_tex_run),
        vec![Assertion::ExitCode(0), counter_eq("error_named", 1)],
        vec![LogExpect::span_present(
            "e2e.failure",
            vec![
                FieldPred::str_eq("error_kind", "TexError::Math"),
                FieldPred::str_eq("names_construct", "true"),
                FieldPred::str_eq("named_refusal", "true"),
            ],
        )],
    ));
    specs.push(spec(
        "failure_path.cache_corrupt_recovers.v1",
        ScenarioClass::FailurePath,
        Surface::RustApi,
        Invocation::new(failure_cache_run),
        vec![
            Assertion::ExitCode(0),
            counter_eq("cache_corrupt_miss", 1),
            counter_eq("cache_recovery_ok", 1),
        ],
        vec![LogExpect::span_present(
            "e2e.failure",
            vec![
                FieldPred::str_eq("cache_corrupt_miss", "true"),
                FieldPred::str_eq("cache_corrupt_evicted", "true"),
            ],
        )],
    ));
    specs.push(spec(
        "failure_path.cli_quality_exclusive_named.v1",
        ScenarioClass::FailurePath,
        Surface::CliInProcess,
        Invocation::new(failure_cli_run),
        vec![
            Assertion::ExitCode(2),
            counter_eq("cli_rule_named", 1),
            Assertion::NdjsonSchema,
        ],
        vec![LogExpect::span_present(
            "e2e.failure",
            vec![
                FieldPred::str_eq("rule", "quality-exclusive"),
                FieldPred::str_eq("rule_named", "true"),
                FieldPred::str_eq("stderr_empty", "true"),
            ],
        )],
    ));

    // LIFECYCLE DRILLS.
    specs.push(spec(
        "lifecycle.construct_snapshot_transform_snapshot.v1",
        ScenarioClass::LifecycleDrill,
        Surface::RustApi,
        Invocation::new(lifecycle_construct_run),
        vec![
            Assertion::ExitCode(0),
            counter_ge("lifecycle_members", 1),
            counter_ge("lifecycle_points_pre", 1),
            counter_ge("lifecycle_points_post", 1),
            Assertion::GoldenLock {
                suite: E2E_SUITE,
                scope: Scope::Certified,
            },
        ],
        vec![
            LogExpect::span_present(
                e2e::spans::SCENE_CONSTRUCT,
                vec![
                    FieldPred::str_eq("scene", "circle_tex_label.v1"),
                    FieldPred::u64_ge("members", 1),
                ],
            ),
            LogExpect::event_order(e2e::spans::SCENE_CONSTRUCT, "e2e.lifecycle.transform"),
        ],
    ));
    specs.push(spec(
        "lifecycle.public_fmn_facade_executes.v1",
        ScenarioClass::LifecycleDrill,
        Surface::RustApi,
        Invocation::new(public_facade_run),
        vec![
            Assertion::ExitCode(0),
            counter_eq("public_facade_plays", 2),
            counter_eq("public_facade_captures", 3),
        ],
        vec![LogExpect::span_present(
            e2e::spans::SCENE_CONSTRUCT,
            vec![
                FieldPred::str_eq("scene", "public_facade_circle_shift"),
                FieldPred::u64_eq("captures", 3),
            ],
        )],
    ));
    specs.push(spec(
        "lifecycle.journal_roundtrip_replay.v1",
        ScenarioClass::LifecycleDrill,
        Surface::RustApi,
        Invocation::new(lifecycle_journal_run),
        vec![
            Assertion::ExitCode(0),
            counter_eq("journal_roundtrip_equal", 1),
            counter_eq("journal_replay_reuse", 3),
            counter_eq("journal_mismatch_detected", 1),
        ],
        vec![LogExpect::span_present(
            "e2e.lifecycle.journal",
            vec![
                FieldPred::str_eq("roundtrip_equal", "true"),
                FieldPred::u64_ge("replay_reuse", 3),
            ],
        )],
    ));
    specs.push(spec(
        "lifecycle.pg5_definition_direct_schedule.v1",
        ScenarioClass::LifecycleDrill,
        Surface::RustApi,
        Invocation::new(pg5_direct_schedule_run),
        vec![
            Assertion::ExitCode(0),
            counter_eq("pg5_direct_threads", 3),
            counter_eq("pg5_direct_mismatches", 0),
        ],
        vec![LogExpect::span_present(
            "performance.pg5.direct_schedule",
            vec![
                FieldPred::str_eq("scene", "circle_tex_label.v1"),
                FieldPred::str_eq("direct_threads", "1,4,16"),
                FieldPred::u64_eq("direct_mismatches", 0),
            ],
        )],
    ));
    specs.push(spec(
        "lifecycle.pg6_arena_reuse_zero_allocations.v1",
        ScenarioClass::LifecycleDrill,
        Surface::RustApi,
        Invocation::new(pg6_allocation_reuse_run),
        vec![
            Assertion::ExitCode(0),
            counter_eq("pg6_digest_equal", 1),
            counter_eq("pg6_measured_allocations", 0),
        ],
        vec![LogExpect::span_present(
            "performance.pg6.allocations",
            vec![
                FieldPred::str_eq("scene", "circle_tex_label.v1"),
                FieldPred::u64_eq("measured_allocations", 0),
            ],
        )],
    ));
    specs.push(
        spec(
            "lifecycle.pg6_peak_gallery_4k_3d.v1",
            ScenarioClass::LifecycleDrill,
            Surface::RustApi,
            Invocation::new(pg6_peak_gallery_run),
            vec![
                Assertion::ExitCode(0),
                counter_eq("pg6_peak_frames", PG6_PEAK_CASE_COUNT as u64),
                counter_eq("pg6_peak_distinct_digests", 1),
            ],
            vec![LogExpect::span_present(
                "performance.pg6.peak_gallery",
                vec![
                    FieldPred::u64_eq("frames", PG6_PEAK_CASE_COUNT as u64),
                    FieldPred::u64_eq("width", u64::from(PG6_PEAK_WIDTH)),
                    FieldPred::u64_eq("height", u64::from(PG6_PEAK_HEIGHT)),
                    FieldPred::bool_eq("distinct_digests", true),
                    FieldPred::u64_ge("max_preparation_bytes", 1),
                ],
            )],
        )
        .tier(Tier::Full),
    );

    // LOG ASSERTIONS (machinery proofs).
    specs.push(spec(
        "logs.preflight_typesets_before_first_frame.v1",
        ScenarioClass::LifecycleDrill,
        Surface::RustApi,
        Invocation::new(logs_preflight_run),
        vec![
            Assertion::ExitCode(0),
            counter_eq("typesets", 2),
            counter_eq("preflight_before_first_frame", 1),
        ],
        vec![
            LogExpect::span_present(
                e2e::spans::PREFLIGHT,
                vec![
                    FieldPred::u64_ge("typesets", 2),
                    FieldPred::str_eq("before_first_frame", "true"),
                ],
            ),
            LogExpect::event_order(e2e::spans::PREFLIGHT, e2e::spans::RENDER_FRAME),
            LogExpect::counter_ge(e2e::counters::TYPESETS, 2),
        ],
    ));
    specs.push(spec(
        "logs.segment_purity_recorded.v1",
        ScenarioClass::DeterminismDrill,
        Surface::RustApi,
        Invocation::new(logs_purity_run),
        vec![Assertion::ExitCode(0), counter_eq("segments_classified", 2)],
        vec![
            LogExpect::span_present(
                e2e::spans::PURITY,
                vec![
                    FieldPred::str_eq("segment", "play"),
                    FieldPred::str_eq("purity", "pure"),
                ],
            ),
            LogExpect::span_present(
                e2e::spans::PURITY,
                vec![
                    FieldPred::str_eq("segment", "wait"),
                    FieldPred::str_eq("purity", "stateful"),
                ],
            ),
        ],
    ));
    specs.push(spec(
        "logs.engine_identity_certified_reproducible.v1",
        ScenarioClass::RenderMatrix,
        Surface::RustApi,
        Invocation::new(logs_engine_identity_run),
        vec![
            Assertion::ExitCode(0),
            counter_eq("engine_certified_frames", 1),
        ],
        vec![
            LogExpect::span_present(
                e2e::spans::ENGINE,
                vec![
                    FieldPred::str_eq("engine", "certified_cpu"),
                    FieldPred::str_eq("reproducible", "true"),
                ],
            ),
            LogExpect::span_present(
                e2e::spans::EXECUTION_PLAN,
                vec![
                    FieldPred::str_eq("tuning_source", "certified_profile"),
                    FieldPred::str_eq("determinism", "certified"),
                ],
            ),
            LogExpect::event_order(e2e::spans::EXECUTION_PLAN, e2e::spans::RENDER_FRAME),
        ],
    ));

    // ------------------------------------------------------------------
    // Deliberately-injected regression drills: one per seeded class. The
    // runner drives these RED and requires the repro bundle plus the log
    // artifact to appear, tagged with the class.
    // ------------------------------------------------------------------
    specs.push(
        matrix_spec(
            &PRESETS[0],
            SinkKind::Y4m,
            true,
            Some("render_matrix.low.y4m.certified.drill_golden.v1"),
        )
        .regression(RegressionKind::GoldenDrift),
    );
    specs.push(
        spec(
            "determinism.repeat_byte_identical.drill_log.v1",
            ScenarioClass::DeterminismDrill,
            Surface::RustApi,
            Invocation::new(determinism_repeat_run),
            vec![Assertion::ExitCode(0), counter_eq("repeat_byte_equal", 1)],
            vec![LogExpect::counter_ge("repeat_byte_equal", 1)],
        )
        .regression(RegressionKind::LogExpectation),
    );
    specs.push(
        spec(
            "failure_path.tex_unsupported_construct_named.drill_log.v1",
            ScenarioClass::FailurePath,
            Surface::RustApi,
            Invocation::new(failure_tex_run),
            vec![Assertion::ExitCode(0), counter_eq("error_named", 1)],
            vec![LogExpect::span_present(
                "e2e.failure",
                vec![FieldPred::str_eq("error_kind", "TexError::Math")],
            )],
        )
        .regression(RegressionKind::LogExpectation),
    );
    specs.push(
        spec(
            "lifecycle.construct_snapshot_transform_snapshot.drill_golden.v1",
            ScenarioClass::LifecycleDrill,
            Surface::RustApi,
            Invocation::new(lifecycle_construct_run),
            vec![
                Assertion::ExitCode(0),
                Assertion::GoldenLock {
                    suite: E2E_SUITE,
                    scope: Scope::Certified,
                },
            ],
            Vec::new(),
        )
        .regression(RegressionKind::GoldenDrift),
    );

    specs
}

// ---------------------------------------------------------------------------
// Test entry points
// ---------------------------------------------------------------------------

/// The fast tier: every non-drill `Tier::Fast` scenario runs green
/// per-commit.
#[test]
fn fast_tier_scenarios_pass() {
    let runner = Runner::from_env();
    let scenarios: Vec<ScenarioSpec> = catalog()
        .into_iter()
        .filter(|scenario| scenario.tier == Tier::Fast && scenario.regression.is_none())
        .collect();
    assert!(
        scenarios.len() >= 10,
        "the fast tier thinned out: {} scenarios",
        scenarios.len()
    );
    let reports = runner.run_all(scenarios);
    let failures: Vec<String> = reports
        .iter()
        .filter(|report| !report.is_pass())
        .map(|report| report.summary())
        .collect();
    assert!(failures.is_empty(), "{}", failures.join("\n\n"));
}

/// The full matrix: env-gated for the nightly budget (R22). With the
/// gate off every full-tier scenario must SKIP (never silently pass,
/// never fail); with it on every one must run green.
#[test]
fn full_tier_scenarios_pass_when_enabled() {
    let runner = Runner::from_env();
    let scenarios: Vec<ScenarioSpec> = catalog()
        .into_iter()
        .filter(|scenario| scenario.tier == Tier::Full && scenario.regression.is_none())
        .collect();
    assert!(
        scenarios.len() >= 10,
        "the full tier thinned out: {} scenarios",
        scenarios.len()
    );
    let reports = runner.run_all(scenarios);
    if e2e::full_matrix_from_env() {
        let failures: Vec<String> = reports
            .iter()
            .filter(|report| !report.is_pass())
            .map(|report| report.summary())
            .collect();
        assert!(failures.is_empty(), "{}", failures.join("\n\n"));
    } else {
        for report in &reports {
            assert!(
                matches!(report.status, Status::Skipped(_)),
                "full-tier scenario ran or failed without {FULL_TIER_ENV}=1: {}",
                report.summary()
            );
        }
    }
}

/// The injected-regression drills: each goes red with useful logs, and
/// the repro bundle plus log artifact appear on disk, tagged with the
/// class.
#[test]
fn injected_regression_drills_go_red_with_repro_bundles() {
    let runner = Runner::from_env();
    let drills: Vec<ScenarioSpec> = catalog()
        .into_iter()
        .filter(|scenario| scenario.regression.is_some())
        .collect();
    let classes: Vec<ScenarioClass> = drills.iter().map(|scenario| scenario.class).collect();
    assert_eq!(
        drills.len(),
        4,
        "one injected regression drill per seeded class"
    );
    let reports = runner.run_all(drills);
    assert_eq!(reports.len(), classes.len());
    for (report, class) in reports.iter().zip(classes) {
        assert!(
            report.went_red(),
            "drill {} did not go red: {}",
            report.scenario,
            report.summary()
        );
        let repro = report
            .repro_bundle()
            .expect("a red drill carries its repro bundle path");
        let log = report
            .log_artifact()
            .expect("a red drill carries its log artifact path");
        assert!(
            repro.exists(),
            "repro bundle missing at {}",
            repro.display()
        );
        assert!(log.exists(), "log artifact missing at {}", log.display());
        match &report.status {
            Status::RegressionConfirmed { failure } => {
                assert_eq!(
                    failure.class, class,
                    "drill {} is tagged with the wrong class",
                    report.scenario
                );
            }
            other => assert!(
                matches!(other, Status::RegressionConfirmed { .. }),
                "drill {} concluded {} instead of RegressionConfirmed",
                report.scenario,
                other.tag()
            ),
        }
    }
}

/// The runner's own NDJSON log artifact validates against its declared
/// schema — the harness's self-check, run against a real scenario's real
/// log output on disk.
#[test]
fn runner_log_artifact_validates_against_declared_schema() {
    let runner = Runner::from_env();
    let scenario = catalog()
        .into_iter()
        .find(|scenario| scenario.name == "logs.preflight_typesets_before_first_frame.v1")
        .expect("the preflight scenario is registered");
    let report = runner.run(scenario);
    assert!(report.is_pass(), "{}", report.summary());
    let log = report
        .log_artifact()
        .expect("a passing run carries its log artifact");
    e2e::validate_log_artifact(log)
        .expect("the run's NDJSON log artifact validates against the harness's declared schema");
}

/// Catalog invariants: unique path-safe names, class coverage, a drill
/// per class, and no Pending-surface stubs.
#[test]
fn catalog_invariants_hold() {
    let scenarios = catalog();
    assert!(scenarios.len() >= 25, "catalog thinned out");

    let mut names = std::collections::BTreeSet::new();
    for scenario in &scenarios {
        assert!(
            names.insert(scenario.name),
            "duplicate scenario name {}",
            scenario.name
        );
        assert!(
            scenario
                .name
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || "._-".contains(c)),
            "scenario name {} violates the artifact-name charset",
            scenario.name
        );
    }

    for class in [
        ScenarioClass::RenderMatrix,
        ScenarioClass::DeterminismDrill,
        ScenarioClass::FailurePath,
        ScenarioClass::LifecycleDrill,
    ] {
        let seeded = scenarios
            .iter()
            .filter(|s| s.class == class && s.regression.is_none())
            .count();
        let drills = scenarios
            .iter()
            .filter(|s| s.class == class && s.regression.is_some())
            .count();
        assert!(seeded >= 2, "{class:?} has {seeded} scenarios");
        assert_eq!(drills, 1, "{class:?} must carry exactly one drill");
    }

    // Pending surfaces are declared, never stubbed: nothing registers against
    // them until their owning bead lands real behavior.
    assert!(
        scenarios
            .iter()
            .all(|s| !matches!(s.surface, Surface::PythonPending)),
        "a Pending surface acquired a stub scenario"
    );
    assert!(
        scenarios
            .iter()
            .any(|scenario| scenario.surface == Surface::StudioInProcess),
        "Studio lost its production composition scenario"
    );
}
