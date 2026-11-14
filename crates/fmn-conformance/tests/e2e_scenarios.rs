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
//!   `fmn_cli`'s in-process runner; certified Metal selection exits 3 before
//!   creating its requested output path.
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
//! The Python standard PNG surface is live through its feature-gated embedded
//! CPython adapter. Studio is live through the production CLI composition root;
//! both have registered frame scenarios, while unsupported Python modes remain
//! precise capability refusals;
//! Studio also has a mandatory native subprocess lifecycle row over Cargo's
//! exact shipping CLI artifact, authenticated loopback HTTP and multipart PNG.
//! Every seeded class carries one deliberately-injected
//! regression drill (`RegressionKind`): the runner drives the scenario
//! red, and the repro bundle plus log artifact must appear.

#[allow(dead_code)]
#[path = "../../fmn/examples/live_bar_chart.rs"]
mod live_bar_chart_example;
#[path = "../../fmn/tests/support/obj_smoothing.rs"]
mod obj_smoothing;
#[path = "support/path_motion.rs"]
mod path_motion;
#[path = "support/path_observation.rs"]
mod path_observation;
#[allow(dead_code)]
#[path = "support/semantic_routes.rs"]
mod semantic_routes;
#[path = "support/studio_mjpeg.rs"]
mod studio_mjpeg;
#[path = "support/studio_native_resume.rs"]
mod studio_native_resume;
#[path = "support/studio_pure_seek.rs"]
mod studio_pure_seek;
#[path = "support/studio_wait_tail.rs"]
mod studio_wait_tail;

// fm-longform-native-e2e-hoeffding-l59d: the Hoeffding's D explainer
// (crates/fmn/examples/hoeffding_d), compiled here from its own sources so
// both tiers render the real chapters through `fmn::render`. The chapters
// name their siblings as `crate::kit` etc., hence the crate-root modules.
#[allow(dead_code)]
#[path = "../../fmn/examples/hoeffding_d/chapters/mod.rs"]
mod chapters;
#[allow(dead_code)]
#[path = "../../fmn/examples/hoeffding_d/kit.rs"]
mod kit;
#[allow(dead_code)]
#[path = "../../fmn/examples/hoeffding_d/narration.rs"]
mod narration;
#[allow(dead_code)]
#[path = "../../fmn/examples/hoeffding_d/sound.rs"]
mod sound;
#[allow(dead_code)]
#[path = "../../fmn/examples/hoeffding_d/stats.rs"]
mod stats;

use fmn_anim::animation::Animation as _;
use fmn_anim::{FramePacket, MoveAlongPath, RateFunc, Timeline};
use fmn_conformance::e2e::{
    self, Assertion, FieldPred, Invocation, LogEvent, LogExpect, RegressionKind, RunCtx,
    RunOutcome, Runner, ScenarioClass, ScenarioError, ScenarioSpec, Status, Surface, Tier,
};
use fmn_conformance::golden::Scope;
use fmn_conformance::perf::{BenchmarkKey, GateId, MetricUnit};
use fmn_conformance::perf_host::measurement_identity;
use fmn_conformance::perf_pg5::{PG5_DIRECT_THREADS, Pg5Definition, pg5_identity};
use fmn_conformance::perf_pg6::{PG6_THREADS, Pg6Definition, pg6_identity};
use fmn_conformance::perf_pg6_peak::{
    PG6_PEAK_CASE_COUNT, PG6_PEAK_HEIGHT, PG6_PEAK_WIDTH, Pg6PeakDefinition,
    render_locked_gallery_once,
};
use fmn_conformance::scene_goldens::{self, TILING};
use fmn_core::color::Srgb;
use fmn_core::constants::{
    BLUE_C, BOTTOM, DOWN, FRAME_X_RADIUS, FRAME_Y_RADIUS, LEFT, LEFT_SIDE, RED_C, RIGHT,
    RIGHT_SIDE, TEAL_B, TOP, UL, UP, WHITE, YELLOW,
};
use fmn_core::rng::RngRoot;
use fmn_frame::convert::{rgba_to_nv12, rgba16f_to_rgba8};
use fmn_frame::{ChromaSiting, ColorRange, FrameBuffer, FrameLayout, PixelFormat};
use fmn_hash::sha256;
use fmn_library::style::VStyle;
use fmn_library::vmobject::VMobject;
use fmn_library::{Circle, FunctionGraph, NumberPlane};
use fmn_mobject::animate::AnimateArgs;
use fmn_mobject::record::{RecordBuffer, RecordSchema};
use fmn_mobject::{Mob, Mobject, Stage};
use fmn_output::sinks::{
    NativeArtifactKind, NativeArtifactReport, PngSink, PngSinkConfig, PngTarget, SinkLimits,
    Y4mSink, Y4mSinkConfig,
};
use fmn_output::{ClosureItem, FrameSink, ManifestMode, ProvenanceManifest, SinkWrite};
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
use fmn_tex::{Prim, TexError};
use std::io::{BufRead, Read, Write};
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
            y_up: true,
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
    scenario_dir_in(&base, scenario, &RUN_SUFFIX)
}

fn scenario_dir_in(
    base: &std::path::Path,
    scenario: &str,
    sequence: &AtomicU64,
) -> Result<PathBuf, ScenarioError> {
    let mut builder = std::fs::DirBuilder::new();
    builder.recursive(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt as _;

        builder.mode(0o700);
    }
    for _ in 0..1024 {
        let suffix = sequence.fetch_add(1, Ordering::Relaxed);
        let dir = base.join(format!(
            "fmn_e2e_{scenario}.{}.{}",
            std::process::id(),
            suffix
        ));
        match builder.create(&dir) {
            Ok(()) => return Ok(dir),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => {
                return Err(fail(format!("create {}: {error}", dir.display())));
            }
        }
    }
    Err(fail("could not claim a fresh private scenario directory"))
}

#[test]
fn scenario_directories_are_private_and_never_adopt_existing_paths() {
    let base = scenario_dir("directory_ownership").expect("private test parent");
    let first =
        scenario_dir_in(&base, "collision", &AtomicU64::new(0)).expect("first exclusive directory");
    let sentinel = first.join("retained-source");
    std::fs::write(&sentinel, b"must remain unchanged").expect("retained ownership witness");
    let second = scenario_dir_in(&base, "collision", &AtomicU64::new(0))
        .expect("skip existing directory without adopting it");
    assert_ne!(first, second);
    assert_eq!(
        std::fs::read(&sentinel).expect("existing data retained"),
        b"must remain unchanged"
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;

        for directory in [&base, &first, &second] {
            let mode = std::fs::metadata(directory)
                .expect("owned directory metadata")
                .permissions()
                .mode();
            assert_eq!(
                mode & 0o077,
                0,
                "private directory: {}",
                directory.display()
            );
            assert_eq!(mode & 0o700, 0o700, "owner access retained");
        }
    }
}

fn sink_limits(frames: u64) -> Result<SinkLimits, ScenarioError> {
    SinkLimits::new(16, 1 << 30, 1 << 34, 1 << 34)
        .and_then(|limits| limits.requiring_exact_frames(frames))
        .map_err(|error| fail(format!("sink limits: {error}")))
}

/// fm-hyqz: the PNG-sequence sink encodes frames concurrently. Twelve
/// certified frames publish byte-identical at 1 and 4 encoder workers (the
/// same members and tree digest), and the run logs the sink's frame
/// concurrency, its in-flight byte bound, and the encoder lanes the profile
/// saw. Lane counts depend on scheduling, so they are logged, not asserted.
fn png_sequence_frame_concur…200152 tokens truncated…  counter_eq("cli_ffmpeg_admitted_timeout", 21_600),
            counter_eq("cli_ffmpeg_admitted_artifact", 8 << 30),
            counter_eq("cli_ffmpeg_admitted_input", 8 << 30),
            Assertion::NdjsonSchema,
        ],
        vec![LogExpect::span_present(
            "e2e.failure.cli_ffmpeg_limits",
            vec![
                FieldPred::u64_eq("refused", 4),
                FieldPred::u64_eq("tool_calls", 0),
                FieldPred::u64_eq("admitted_timeout_seconds", 21_600),
            ],
        )],
    ));
    #[cfg(unix)]
    specs.push(spec(
        "failure_path.encoder_help_process_evidence.v1",
        ScenarioClass::FailurePath,
        Surface::RustApi,
        Invocation::new(failure_encoder_help_evidence_run),
        vec![
            Assertion::ExitCode(0),
            counter_eq("encoder_help_terminal_modes", 3),
        ],
        vec![LogExpect::span_present(
            "e2e.failure.encoder_help",
            vec![
                FieldPred::u64_eq("timeout_ms", 2_000),
                FieldPred::u64_eq("log_cap", 65_536),
            ],
        )],
    ));
    specs.push(spec(
        "failure_path.facade_video_requires_ffmpeg_capability.v1",
        ScenarioClass::FailurePath,
        Surface::RustApi,
        Invocation::new(failure_facade_video_capability_run),
        vec![
            Assertion::ExitCode(0),
            counter_eq("facade_video_refusal_named", 1),
            counter_ge("facade_native_frames", 1),
        ],
        vec![LogExpect::span_present(
            "e2e.failure",
            vec![
                FieldPred::str_eq("rule", "facade-video-capability"),
                FieldPred::str_eq("refusal_named", "true"),
                FieldPred::str_eq("scene_ran", "false"),
                FieldPred::str_eq("published", "false"),
            ],
        )],
    ));
    specs.push(spec(
        "failure_path.cli_window_flag_refused_timing_kept.v1",
        ScenarioClass::FailurePath,
        Surface::CliInProcess,
        Invocation::new(failure_cli_window_flag_run),
        vec![
            Assertion::ExitCode(0),
            counter_eq("cli_window_flag_refused", 1),
            counter_eq("cli_plain_render_frames", 23),
        ],
        vec![LogExpect::span_present(
            "e2e.failure",
            vec![
                FieldPred::str_eq("rule", "window-flag-on-file-render"),
                FieldPred::u64_eq("refused_code", 4),
                FieldPred::str_eq("published_nothing", "true"),
                FieldPred::u64_eq("plain_frames", 23),
            ],
        )],
    ));
    specs.push(spec(
        "failure_path.cli_certified_metal_prepublication_refusal.v1",
        ScenarioClass::FailurePath,
        Surface::CliInProcess,
        Invocation::new(failure_cli_certified_metal_run),
        vec![
            Assertion::ExitCode(3),
            counter_eq("cli_certified_metal_refused", 1),
        ],
        vec![LogExpect::span_present(
            "e2e.failure",
            vec![
                FieldPred::str_eq("contract", "certified-cpu-only"),
                FieldPred::str_eq("contract_named", "true"),
                FieldPred::str_eq("stderr_empty", "true"),
                FieldPred::str_eq("output_absent", "true"),
            ],
        )],
    ));
    specs.push(spec(
        "lifecycle.cli_certification_admission.v1",
        ScenarioClass::LifecycleDrill,
        Surface::CliInProcess,
        Invocation::new(cli_certification_admission_run),
        vec![
            Assertion::ExitCode(0),
            counter_eq("native_standard_frames", 3),
            counter_eq("native_certification_contract", 1),
        ],
        vec![LogExpect::span_present(
            "e2e.cli.certification_admission",
            vec![
                FieldPred::str_eq("certified_supported", truth(CLI_CERTIFIED_PLATFORM)),
                FieldPred::str_eq("standard_frames", "3"),
                FieldPred::str_eq("policy_agrees", "true"),
            ],
        )],
    ));
    specs.push(spec(
        if CLI_CERTIFIED_PLATFORM {
            "determinism.certified_manifest_compare.v1"
        } else {
            "determinism.standard_manifest_compare.v1"
        },
        ScenarioClass::DeterminismDrill,
        Surface::CliInProcess,
        Invocation::new(determinism_manifest_compare_run),
        vec![
            Assertion::ExitCode(0),
            counter_eq(
                if CLI_CERTIFIED_PLATFORM {
                    "certified_manifest_agree"
                } else {
                    "standard_manifest_agree"
                },
                1,
            ),
            counter_eq(
                if CLI_CERTIFIED_PLATFORM {
                    "certified_manifest_input_distinguished"
                } else {
                    "standard_manifest_input_distinguished"
                },
                1,
            ),
        ],
        vec![LogExpect::span_present(
            "e2e.determinism.manifest",
            vec![
                FieldPred::str_eq(
                    "mode",
                    if CLI_CERTIFIED_PLATFORM {
                        "certified"
                    } else {
                        "standard"
                    },
                ),
                FieldPred::str_eq("same_input_agree", "true"),
                FieldPred::str_eq("other_input_semantic_equal", "false"),
                FieldPred::str_eq("platform_record", "true"),
                FieldPred::str_eq("fonts_recorded", "true"),
                FieldPred::str_eq("build_identity_named", "true"),
            ],
        )],
    ));
    specs.push(spec(
        "failure_path.host_qualification_requires_live_token.v1",
        ScenarioClass::FailurePath,
        Surface::RustApi,
        Invocation::new(failure_host_attestation_run),
        vec![
            Assertion::ExitCode(0),
            counter_eq("host_qualification_refused", 1),
        ],
        vec![LogExpect::span_present(
            "performance.host_attestation",
            vec![
                FieldPred::str_eq("token_present", "false"),
                FieldPred::str_eq("bare_metal", "false"),
                FieldPred::str_eq("isolated", "false"),
                FieldPred::str_eq("caller_claim_refused", "true"),
            ],
        )],
    ));

    // LIFECYCLE DRILLS.
    specs.push(spec(
        "lifecycle.snapshot_handle_safety.v1",
        ScenarioClass::LifecycleDrill,
        Surface::RustApi,
        Invocation::new(snapshot_handle_safety_run),
        vec![
            Assertion::ExitCode(0),
            counter_eq("snapshot_handle_restores", 5),
            counter_eq("snapshot_handle_updater_calls", 4),
            counter_eq("snapshot_handle_frozen_branches", 4),
        ],
        vec![LogExpect::span_present(
            "e2e.lifecycle.snapshot_handles",
            vec![
                FieldPred::u64_eq("restores", 5),
                FieldPred::u64_eq("discarded_objects", 8),
                FieldPred::u64_eq("updater_calls", 4),
                FieldPred::u64_eq("frozen_branches", 4),
            ],
        )],
    ));
    specs.push(spec(
        "lifecycle.native_positional_wag.v1",
        ScenarioClass::LifecycleDrill,
        Surface::RustApi,
        Invocation::new(native_positional_lifecycle_run),
        vec![
            Assertion::ExitCode(0),
            counter_eq("native_positional_samples", 2),
            counter_eq("native_positional_restores", 1),
        ],
        vec![LogExpect::span_present(
            "e2e.lifecycle.native_positional",
            vec![
                FieldPred::u64_eq("portrait_width_milli", 4_500),
                FieldPred::u64_eq("seek_samples", 2),
                FieldPred::u64_eq("live_frame_width_milli", 12_000),
                FieldPred::u64_eq("snapshot_frame_width_milli", 4_500),
                FieldPred::str_eq("frame_restored", "true"),
            ],
        )],
    ));
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
            counter_ge("lifecycle_shared_entries", 1),
            counter_ge("lifecycle_detached_entries", 1),
            counter_eq("lifecycle_frozen_frames_equal", 2),
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
            LogExpect::span_present(
                "e2e.lifecycle.snapshot_sharing",
                vec![
                    FieldPred::u64_ge("shared_entries", 1),
                    FieldPred::u64_ge("detached_entries", 1),
                    FieldPred::u64_eq("frozen_frames_equal", 2),
                ],
            ),
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
    #[cfg(target_os = "linux")]
    specs.push(spec(
        "lifecycle.linux_resource_limits.v1",
        ScenarioClass::LifecycleDrill,
        Surface::RustApi,
        Invocation::new(linux_resource_limits_run),
        vec![
            Assertion::ExitCode(0),
            counter_eq("linux_resource_modes", 2),
            counter_eq("linux_resource_schedules", 4),
            counter_eq("linux_resource_frames", 12),
        ],
        vec![LogExpect::span_present(
            "e2e.lifecycle.linux_resource_limits",
            vec![
                FieldPred::str_eq("schedule", "auto"),
                FieldPred::str_eq("allowed_cpus", "2-5"),
                FieldPred::u64_eq("cpu_quota", 2),
                FieldPred::u64_eq("render_workers", 2),
                FieldPred::u64_eq("frames_in_flight", 1),
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
    // Parity drills (fm-parity-drill-scenarios-hzng): one fast-tier drill
    // per Parity Ledger ruling family — C-5, C-17, C-15, C-16, BN-03, and
    // the §11.3 span map — each asserting the CORRECTED behavior
    // end-to-end so the Reference-buggy answer turns the drill red.
    // ------------------------------------------------------------------
    specs.push(spec(
        "parity.updater_call_runs_once.v1",
        ScenarioClass::ParityDrill,
        Surface::RustApi,
        Invocation::new(parity_updater_call_run),
        vec![
            Assertion::ExitCode(0),
            counter_eq("parity_updater_attach_calls", 1),
        ],
        vec![LogExpect::span_present(
            "e2e.parity.updater_call_once",
            vec![
                FieldPred::u64_eq("call_attach_calls", 1),
                FieldPred::u64_eq("dt_attach_calls", 1),
            ],
        )],
    ));
    specs.push(spec(
        "parity.plane_y_unit_size.v1",
        ScenarioClass::ParityDrill,
        Surface::RustApi,
        Invocation::new(parity_plane_y_unit_run),
        vec![
            Assertion::ExitCode(0),
            counter_eq("parity_plane_y_unit_is_own_axis", 1),
        ],
        vec![LogExpect::span_present(
            "e2e.parity.plane_y_unit_size",
            vec![FieldPred::u64_eq("y_unit_milli", 2_000)],
        )],
    ));
    specs.push(spec(
        "parity.function_graph_color.v1",
        ScenarioClass::ParityDrill,
        Surface::RustApi,
        Invocation::new(parity_function_graph_color_run),
        vec![
            Assertion::ExitCode(0),
            counter_eq("parity_function_graph_color_honored", 1),
        ],
        vec![LogExpect::span_present(
            "e2e.parity.function_graph_color",
            vec![
                FieldPred::str_eq("default_yellow", "true"),
                FieldPred::str_eq("requested_honored", "true"),
            ],
        )],
    ));
    specs.push(spec(
        "parity.frame_markers_to_edge.v1",
        ScenarioClass::ParityDrill,
        Surface::RustApi,
        Invocation::new(parity_frame_markers_run),
        vec![
            Assertion::ExitCode(0),
            counter_eq("parity_frame_marker_placements", 5),
        ],
        vec![LogExpect::span_present(
            "e2e.parity.frame_markers",
            vec![
                FieldPred::u64_eq("marker_directions", 4),
                FieldPred::u64_eq("exact_placements", 5),
            ],
        )],
    ));
    specs.push(spec(
        "parity.move_along_path_true_arclength.v1",
        ScenarioClass::ParityDrill,
        Surface::RustApi,
        Invocation::new(parity_move_along_path_run),
        vec![
            Assertion::ExitCode(0),
            counter_eq("parity_arclength_animation_matches", 3),
            counter_eq("parity_arclength_path_materializations", 1),
        ],
        vec![LogExpect::span_present(
            "e2e.parity.move_along_path_arclength",
            vec![
                FieldPred::str_eq("equal_steps", "true"),
                FieldPred::u64_eq("animation_matches", 3),
                FieldPred::u64_eq("path_materializations", 1),
            ],
        )],
    ));
    specs.push(spec(
        "parity.tex_formula_strip.v1",
        ScenarioClass::ParityDrill,
        Surface::RustApi,
        Invocation::new(parity_tex_formula_strip_run),
        vec![Assertion::ExitCode(0), counter_eq("tex_strip_formulas", 10)],
        vec![LogExpect::span_present(
            "e2e.parity.tex_formula_strip",
            vec![FieldPred::u64_eq("formulas", 10)],
        )],
    ));
    specs.push(spec(
        "parity.tex_span_map_t2c.v1",
        ScenarioClass::ParityDrill,
        Surface::RustApi,
        Invocation::new(parity_tex_span_map_run),
        vec![
            Assertion::ExitCode(0),
            counter_eq("parity_t2c_source_identity", 1),
        ],
        vec![LogExpect::span_present(
            "e2e.parity.tex_span_map_t2c",
            vec![
                FieldPred::u64_eq("x_occurrences", 2),
                FieldPred::u64_eq("plus_occurrences", 2),
            ],
        )],
    ));
    specs.push(spec(
        "parity.tex_ams.v1",
        ScenarioClass::ParityDrill,
        Surface::RustApi,
        Invocation::new(parity_tex_ams_run),
        vec![
            Assertion::ExitCode(0),
            counter_eq("tex_ams_formulas", 7),
            counter_eq("tex_ams_thread_counts", 3),
        ],
        vec![LogExpect::span_present(
            "e2e.parity.tex_ams",
            vec![
                FieldPred::u64_eq("formulas", 7),
                FieldPred::u64_eq("thread_counts", 3),
            ],
        )],
    ));
    specs.push(spec(
        "parity.tex_cm_recipes.v1",
        ScenarioClass::ParityDrill,
        Surface::RustApi,
        Invocation::new(parity_tex_cm_recipes_run),
        vec![
            Assertion::ExitCode(0),
            counter_eq("tex_cm_recipe_formulas", 3),
        ],
        vec![LogExpect::span_present(
            "e2e.parity.tex_cm_recipes",
            vec![
                FieldPred::u64_eq("formulas", 3),
                FieldPred::u64_eq("integral_source_parts", 2),
                FieldPred::u64_eq("radical_pieces", 5),
            ],
        )],
    ));
    specs.push(spec(
        "lifecycle.family_reversal_end_handle.v1",
        ScenarioClass::LifecycleDrill,
        Surface::RustApi,
        Invocation::new(lifecycle_family_reversal_run),
        vec![
            Assertion::ExitCode(0),
            counter_eq("family_reversal_pixels_equal", 1),
        ],
        vec![LogExpect::span_present(
            "e2e.lifecycle.family_reversal",
            vec![
                FieldPred::u64_eq("contours", 1),
                FieldPred::u64_eq("pixels_equal", 1),
                FieldPred::u64_eq("planted_pixels_differ", 1),
            ],
        )],
    ));
    specs.push(spec(
        "render_matrix.prismify_compound_cap.v1",
        ScenarioClass::RenderMatrix,
        Surface::RustApi,
        Invocation::new(prismify_compound_cap_run),
        vec![
            Assertion::ExitCode(0),
            counter_eq("prismify_cap_thread_counts", 3),
        ],
        vec![LogExpect::span_present(
            "e2e.render_matrix.prismify_compound_cap",
            vec![
                FieldPred::u64_eq("contours", 2),
                FieldPred::u64_eq("thread_counts", 3),
                FieldPred::u64_eq("gap_alpha", 0),
                FieldPred::u64_eq("planted_pixels_differ", 1),
            ],
        )],
    ));
    specs.push(spec(
        "parity.markdown_inline_math.v1",
        ScenarioClass::ParityDrill,
        Surface::RustApi,
        Invocation::new(parity_markdown_inline_math_run),
        vec![
            Assertion::ExitCode(0),
            counter_eq("parity_markdown_prose_dollars", 3),
        ],
        vec![LogExpect::span_present(
            "e2e.parity.markdown_inline_math",
            vec![
                FieldPred::bool_eq("island_is_text_style_tex", true),
                FieldPred::u64_eq("prose_sources", 3),
                FieldPred::u64_eq("prose_sources_literal", 3),
            ],
        )],
    ));
    // ------------------------------------------------------------------
    // fm-3kr / fm-n64 enhanced-surface drills: refusal policy by name,
    // spring-kernel determinism, CSV→Table round-trip.
    // ------------------------------------------------------------------
    specs.push(
        spec(
            "drawings.piano_keyboard.v1",
            ScenarioClass::LifecycleDrill,
            Surface::RustApi,
            Invocation::new(piano_keyboard_run),
            vec![
                Assertion::ExitCode(0),
                counter_eq("piano_keys", 88),
                counter_eq("piano_black_keys", 36),
                counter_eq("piano_centered", 1),
                counter_eq("piano_proportions", 1),
                counter_eq("piano_visible_motion", 1),
            ],
            vec![LogExpect::span_present(
                "e2e.drawings.piano",
                vec![
                    FieldPred::u64_eq("keys", 88),
                    FieldPred::u64_eq("black_keys", 36),
                    FieldPred::str_eq("visible_motion", "true"),
                ],
            )],
        )
        .tier(Tier::Fast),
    );
    specs.push(
        spec(
            "drawings.asset_refusals_named.v1",
            ScenarioClass::LifecycleDrill,
            Surface::RustApi,
            Invocation::new(drawings_asset_refusals_run),
            vec![
                Assertion::ExitCode(0),
                counter_eq("asset_refusals_named", 5),
            ],
            vec![LogExpect::span_present(
                "e2e.drawings.asset_refusal",
                vec![FieldPred::u64_eq("named", 5)],
            )],
        )
        .tier(Tier::Fast),
    );
    specs.push(
        spec(
            "network_graph.spring_seed_determinism.v1",
            ScenarioClass::DeterminismDrill,
            Surface::RustApi,
            Invocation::new(network_graph_layout_determinism_run),
            vec![
                Assertion::ExitCode(0),
                counter_eq("spring_replay_equal", 1),
                counter_eq("spring_seeds_diverge", 1),
            ],
            vec![LogExpect::span_present(
                "e2e.network_graph.spring_determinism",
                vec![FieldPred::u64_eq("seed", 0x005f_3759df)],
            )],
        )
        .tier(Tier::Fast),
    );
    specs.push(spec(
        "lifecycle.native_prepared_images.v1",
        ScenarioClass::LifecycleDrill,
        Surface::RustApi,
        Invocation::new(native_prepared_images_run),
        vec![
            Assertion::ExitCode(0),
            Assertion::FileInventory(vec![
                "prepared_image.png".into(),
                "replacement_image.png".into(),
            ]),
            counter_eq("prepared_image_thread_counts", 3),
            counter_eq("prepared_image_frames", 4),
            counter_eq("prepared_image_receipts", 2),
        ],
        vec![LogExpect::span_present(
            "e2e.native.prepared_images",
            vec![FieldPred::str_eq("immutable_input", "true")],
        )],
    ));
    specs.push(spec(
        "lifecycle.python_graph_animations.v1",
        ScenarioClass::LifecycleDrill,
        Surface::PythonInProcess,
        Invocation::new(python_graph_animations_run),
        vec![
            Assertion::ExitCode(0),
            counter_eq("python_graph_animation_cases", 9),
            counter_eq("python_graph_animation_frames", 78),
            counter_eq("python_graph_animation_thread_replays", 6),
        ],
        vec![LogExpect::span_present(
            "e2e.python.graph_animations",
            vec![FieldPred::str_eq("mode", "standard")],
        )],
    ));
    specs.push(
        spec(
            "lifecycle.native_method_override.v1",
            ScenarioClass::LifecycleDrill,
            Surface::RustApi,
            Invocation::new(native_method_override_run),
            vec![
                Assertion::ExitCode(0),
                counter_eq("method_override_factory_calls", 3),
                counter_eq("method_override_frames", 24),
                counter_eq("method_override_thread_counts", 3),
            ],
            vec![LogExpect::span_present(
                "e2e.native.method_override",
                vec![
                    FieldPred::u64_eq("factory_calls", 3),
                    FieldPred::u64_eq("rendered_frames", 24),
                    FieldPred::u64_eq("thread_counts", 3),
                ],
            )],
        )
        .tier(Tier::Fast),
    );
    specs.push(
        spec(
            "lifecycle.native_pure_camera.v1",
            ScenarioClass::LifecycleDrill,
            Surface::RustApi,
            Invocation::new(native_pure_camera_run),
            vec![
                Assertion::ExitCode(0),
                counter_eq("pure_camera_frames", 36),
                counter_eq("pure_camera_thread_counts", 3),
            ],
            vec![LogExpect::span_present(
                "e2e.native.pure_camera",
                vec![
                    FieldPred::u64_eq("rendered_frames", 36),
                    FieldPred::u64_eq("thread_counts", 3),
                ],
            )],
        )
        .tier(Tier::Fast),
    );
    specs.push(
        spec(
            "lifecycle.graph_animations.v1",
            ScenarioClass::LifecycleDrill,
            Surface::RustApi,
            Invocation::new(graph_animations_run),
            vec![
                Assertion::ExitCode(0),
                counter_eq("graph_animation_thread_counts", 3),
                counter_eq("graph_animation_frames", 66),
            ],
            vec![LogExpect::span_present(
                "e2e.network_graph.animations",
                vec![
                    FieldPred::u64_eq("thread_counts", 3),
                    FieldPred::u64_eq("rendered_frames", 66),
                ],
            )],
        )
        .tier(Tier::Fast),
    );
    specs.push(
        spec(
            "lifecycle.partial_reveal_contours.v1",
            ScenarioClass::LifecycleDrill,
            Surface::RustApi,
            Invocation::new(partial_reveal_contours_run),
            vec![
                Assertion::ExitCode(0),
                counter_eq("partial_reveal_frame_comparisons", 9),
            ],
            vec![LogExpect::span_present(
                "e2e.lifecycle.partial_reveal_contours",
                vec![
                    FieldPred::u64_eq("thread_counts", 3),
                    FieldPred::u64_eq("frame_comparisons", 9),
                ],
            )],
        )
        .tier(Tier::Fast),
    );
    specs.push(
        spec(
            "neural_network.live_training_and_purity.v1",
            ScenarioClass::LifecycleDrill,
            Surface::RustApi,
            Invocation::new(neural_network_live_training_and_purity_run),
            vec![
                Assertion::ExitCode(0),
                counter_eq("feed_forward_matches", 1),
                counter_eq("purity_is_stateful", 1),
                counter_eq("live_pixels_match", 1),
                counter_eq("stable_geometry", 1),
            ],
            vec![LogExpect::span_present(
                "e2e.neural_network.live_training",
                vec![
                    FieldPred::u64_eq("layers", 3),
                    FieldPred::u64_eq("stateful", 1),
                    FieldPred::u64_eq("live_pixels_match", 1),
                ],
            )],
        )
        .tier(Tier::Fast),
    );
    specs.push(
        spec(
            "lifecycle.live_bar_chart.v1",
            ScenarioClass::LifecycleDrill,
            Surface::RustApi,
            Invocation::new(live_bar_chart_run),
            vec![
                Assertion::ExitCode(0),
                counter_eq("bar_chart_thread_counts", 3),
                counter_eq("bar_chart_frames", 42),
            ],
            vec![LogExpect::span_present(
                "e2e.data.live_bar_chart",
                vec![
                    FieldPred::u64_eq("thread_counts", 3),
                    FieldPred::u64_eq("rendered_frames", 42),
                ],
            )],
        )
        .tier(Tier::Fast),
    );
    specs.push(
        spec(
            "data_table.csv_roundtrip.v1",
            ScenarioClass::LifecycleDrill,
            Surface::RustApi,
            Invocation::new(data_table_csv_roundtrip_run),
            vec![
                Assertion::ExitCode(0),
                counter_eq("data_table_headers", 3),
                counter_eq("data_table_rows", 3),
                counter_eq("data_table_builds_identical", 1),
            ],
            vec![LogExpect::span_present(
                "e2e.data_table.roundtrip",
                vec![
                    FieldPred::u64_eq("headers", 3),
                    FieldPred::u64_eq("rows", 3),
                ],
            )],
        )
        .tier(Tier::Fast),
    );
    if cfg!(unix) {
        specs.push(spec(
            "failure.python_worker_receipt_shutdown.v1",
            ScenarioClass::FailurePath,
            Surface::PythonInProcess,
            Invocation::new(python_worker_receipt_shutdown_run),
            vec![
                Assertion::ExitCode(0),
                counter_eq("worker_receipt_checks", 3),
            ],
            vec![LogExpect::span_present(
                "e2e.python.worker_receipt_shutdown",
                vec![FieldPred::u64_eq("checks", 3)],
            )],
        ));
    }
    specs.push(
        spec(
            "lifecycle.python_parallel_batch_checkpoint.v1",
            ScenarioClass::LifecycleDrill,
            Surface::PythonInProcess,
            Invocation::new(python_parallel_batch_checkpoint_run),
            vec![
                Assertion::ExitCode(0),
                counter_eq("parallel_batch_checks", 7),
            ],
            vec![LogExpect::span_present(
                "e2e.python.parallel_batch_checkpoint",
                vec![
                    FieldPred::u64_eq("checks", 7),
                    FieldPred::str_eq("clean", "true"),
                ],
            )],
        )
        .tier(Tier::Fast),
    );
    specs.push(
        spec(
            "lifecycle.frozen_render_packet_pipeline.v1",
            ScenarioClass::LifecycleDrill,
            Surface::RustApi,
            Invocation::new(frozen_render_packet_pipeline_run),
            vec![
                Assertion::ExitCode(0),
                counter_eq("frozen_equal", 1),
                counter_eq("steady_clean", 1),
            ],
            vec![LogExpect::span_present(
                "e2e.render.frozen_retained_pipeline",
                vec![
                    FieldPred::str_eq("frozen_equal", "true"),
                    FieldPred::str_eq("steady_clean", "true"),
                    FieldPred::u64_eq("allocs", 0),
                ],
            )],
        )
        .tier(Tier::Fast),
    );
    specs.push(spec(
        "render_matrix.circular_strokes.standard.v1",
        ScenarioClass::RenderMatrix,
        Surface::RustApi,
        Invocation::new(circular_stroke_render_run),
        vec![
            Assertion::ExitCode(0),
            counter_eq("circular_stroke_frames", 27),
            counter_eq("circular_stroke_admitted_frames", 6),
        ],
        vec![LogExpect::span_present(
            "e2e.render.circular_strokes",
            vec![
                FieldPred::str_eq("engine", "fast_cpu"),
                FieldPred::u64_eq("frames", 27),
                FieldPred::u64_eq("admitted_frames", 6),
                FieldPred::u64_eq("thread_counts", 3),
                FieldPred::u64_eq("aa_policies", 3),
                FieldPred::u64_eq("live_edits", 2),
            ],
        )],
    ));
    specs.push(spec(
        "lifecycle.tile_binned_stroke_render.v1",
        ScenarioClass::LifecycleDrill,
        Surface::RustApi,
        Invocation::new(tile_binned_stroke_render_run),
        vec![
            Assertion::ExitCode(0),
            counter_eq("stroke_thread_counts", 3),
            counter_eq("stroke_retained_replays", 3),
            counter_eq("stroke_edited_frames", 6),
        ],
        vec![LogExpect::span_present(
            "e2e.render.tile_binned_strokes",
            vec![
                FieldPred::u64_eq("thread_counts", 3),
                FieldPred::u64_eq("retained_replays", 3),
                FieldPred::u64_eq("edited_frames", 6),
                FieldPred::u64_ge("visible_pixels", 100),
            ],
        )],
    ));
    specs.push(
        spec(
            "lifecycle.video_color_and_gallery_provenance.v1",
            ScenarioClass::LifecycleDrill,
            Surface::RustApi,
            Invocation::new(video_color_and_gallery_provenance_run),
            vec![
                Assertion::ExitCode(0),
                counter_eq("color_and_provenance_ok", 1),
            ],
            vec![LogExpect::span_present(
                "e2e.governance.color_and_provenance",
                vec![
                    FieldPred::str_eq("pix_fmts_ok", "true"),
                    FieldPred::u64_eq("pending_audits", 0),
                    FieldPred::str_eq("clean_release", "true"),
                ],
            )],
        )
        .tier(Tier::Fast),
    );

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

/// One semantic-oracle log line: route, which frame, the oracle, its
/// fixture, what it required, what it measured, and the verdict.
fn semantic_reading_event(
    route: &str,
    frame: &str,
    oracle: &str,
    fixture: &str,
    expected: &str,
    measured: &str,
    pass: bool,
) -> LogEvent {
    LogEvent::new("e2e.oracle.semantic")
        .field("route", route)
        .field("frame", frame)
        .field("oracle", oracle)
        .field("fixture", fixture)
        .field("expected", expected)
        .field("measured", measured)
        .field("verdict", if pass { "pass" } else { "fail" })
}

/// fm-5wq.46: the semantic witness through every native golden-producing
/// route (library, CLI affine and camera, native and wasm FMTL players).
fn semantic_oracles_native_run(ctx: &mut RunCtx) -> Result<RunOutcome, ScenarioError> {
    let (mut passed, mut rejected) = (0_u64, 0_u64);
    for route in semantic_routes::ROUTES {
        let frame = route().map_err(fail)?;
        let (rendered, flipped) = frame.read();
        for (label, readings) in [("rendered", &rendered), ("planted_vertical_flip", &flipped)] {
            for r in readings {
                ctx.event(semantic_reading_event(
                    frame.route,
                    label,
                    r.oracle,
                    r.fixture,
                    &r.expected,
                    &r.measured,
                    r.pass,
                ));
            }
        }
        match frame.verdict() {
            Ok(()) => {
                passed += 1;
                rejected += 1;
            }
            Err(error) => ctx.event(
                LogEvent::new("e2e.oracle.semantic_failure")
                    .field("route", frame.route)
                    .field("error", error),
            ),
        }
    }
    ctx.counter("semantic_routes_pass", passed);
    ctx.counter("semantic_mirrors_rejected", rejected);
    Ok(RunOutcome::ok()
        .with_counter("semantic_routes_pass", passed)
        .with_counter("semantic_mirrors_rejected", rejected))
}

/// fm-5wq.46: the portal's scene render and camera readback; the Python
/// suite asserts both frames and rejects both planted mirrors.
fn semantic_oracles_portal_run(ctx: &mut RunCtx) -> Result<RunOutcome, ScenarioError> {
    let readings = manimlib::run_portal_gauntlet_semantic_witness().map_err(fail)?;
    let mut routes = std::collections::BTreeSet::new();
    for r in &readings {
        routes.insert(r.route.clone());
        ctx.event(semantic_reading_event(
            &r.route,
            &r.frame,
            &r.oracle,
            "",
            "",
            &r.measured,
            r.pass,
        ));
    }
    ctx.counter("semantic_routes_pass", routes.len() as u64);
    Ok(RunOutcome::ok().with_counter("semantic_routes_pass", routes.len() as u64))
}

// ---------------------------------------------------------------------------
// Test entry points
// ---------------------------------------------------------------------------

#[test]
fn tex_cancel_frak_scenario_passes() {
    let scenario = catalog()
        .into_iter()
        .find(|scenario| scenario.name == "render_matrix.tex_cancel_frak.v1")
        .expect("the Fraktur and cancel scenario is registered");
    let report = Runner::from_env().run(scenario);
    assert!(report.is_pass(), "{}", report.summary());
}

#[test]
fn tex_declared_math_sizes_scenario_passes() {
    let scenario = catalog()
        .into_iter()
        .find(|scenario| scenario.name == "parity.tex_declared_math_sizes.v1")
        .expect("the declared math size scenario is registered");
    let report = Runner::from_env().run(scenario);
    assert!(report.is_pass(), "{}", report.summary());
}

#[test]
fn tex_t1_text_metrics_scenario_passes() {
    let scenario = catalog()
        .into_iter()
        .find(|scenario| scenario.name == "parity.tex_t1_text_metrics.v1")
        .expect("the T1 text metrics scenario is registered");
    let report = Runner::from_env().run(scenario);
    assert!(report.is_pass(), "{}", report.summary());
}

#[test]
fn tex_forced_fraction_styles_scenario_passes() {
    let scenario = catalog()
        .into_iter()
        .find(|scenario| scenario.name == "parity.tex_forced_fraction_styles.v1")
        .expect("the forced fraction styles scenario is registered");
    let report = Runner::from_env().run(scenario);
    assert!(report.is_pass(), "{}", report.summary());
}

#[test]
fn tex_script_metrics_scenario_passes() {
    let scenario = catalog()
        .into_iter()
        .find(|scenario| scenario.name == "parity.tex_script_metrics.v1")
        .expect("the script and prime metrics scenario is registered");
    let report = Runner::from_env().run(scenario);
    assert!(report.is_pass(), "{}", report.summary());
}

#[test]
fn tex_underline_metrics_scenario_passes() {
    let scenario = catalog()
        .into_iter()
        .find(|scenario| scenario.name == "parity.tex_underline_metrics.v1")
        .expect("the underline metrics scenario is registered");
    let report = Runner::from_env().run(scenario);
    assert!(report.is_pass(), "{}", report.summary());
}

#[test]
fn tex_widehat_metrics_scenario_passes() {
    let scenario = catalog()
        .into_iter()
        .find(|scenario| scenario.name == "parity.tex_widehat_metrics.v1")
        .expect("the wide-hat metrics scenario is registered");
    let report = Runner::from_env().run(scenario);
    assert!(report.is_pass(), "{}", report.summary());
}

#[test]
fn tex_widetilde_metrics_scenario_passes() {
    let scenario = catalog()
        .into_iter()
        .find(|scenario| scenario.name == "parity.tex_widetilde_metrics.v1")
        .expect("the wide-tilde metrics scenario is registered");
    let report = Runner::from_env().run(scenario);
    assert!(report.is_pass(), "{}", report.summary());
}

#[test]
fn tex_over_arrow_metrics_scenario_passes() {
    let scenario = catalog()
        .into_iter()
        .find(|scenario| scenario.name == "parity.tex_over_arrow_metrics.v1")
        .expect("the over-arrow metrics scenario is registered");
    let report = Runner::from_env().run(scenario);
    assert!(report.is_pass(), "{}", report.summary());
}

#[test]
fn tex_pifont_cross_scenario_passes() {
    let scenario = catalog()
        .into_iter()
        .find(|scenario| scenario.name == "parity.tex_pifont_cross.v1")
        .expect("the inline pifont cross scenario is registered");
    let report = Runner::from_env().run(scenario);
    assert!(report.is_pass(), "{}", report.summary());
}

#[test]
fn tex_mathml_text_islands_scenario_passes() {
    let scenario = catalog()
        .into_iter()
        .find(|scenario| scenario.name == "parity.tex_mathml_text_islands.v1")
        .expect("the mixed text/MathML scenario is registered");
    let report = Runner::from_env().run(scenario);
    assert!(report.is_pass(), "{}", report.summary());
}

#[test]
fn tex_text_ellipsis_scenario_passes() {
    let scenario = catalog()
        .into_iter()
        .find(|scenario| scenario.name == "parity.tex_text_ellipsis.v1")
        .expect("the native text ellipsis scenario is registered");
    let report = Runner::from_env().run(scenario);
    assert!(report.is_pass(), "{}", report.summary());
}

#[test]
fn textext_display_paragraphs_scenario_passes() {
    let scenario = catalog()
        .into_iter()
        .find(|scenario| scenario.name == "parity.textext_display_paragraphs.v1")
        .expect("the TexText display paragraph scenario is registered");
    let report = Runner::from_env().run(scenario);
    assert!(report.is_pass(), "{}", report.summary());
}

#[test]
fn textext_paragraph_lines_scenario_passes() {
    let scenario = catalog()
        .into_iter()
        .find(|scenario| scenario.name == "parity.textext_paragraph_lines.v1")
        .expect("the TexText paragraph scenario is registered");
    let report = Runner::from_env().run(scenario);
    assert!(report.is_pass(), "{}", report.summary());
}

#[test]
fn tex_constructs_t2_scenario_passes() {
    let scenario = catalog()
        .into_iter()
        .find(|scenario| scenario.name == "render_matrix.tex_constructs_t2.v1")
        .expect("the outside-corpus TeX constructs scenario is registered");
    let report = Runner::from_env().run(scenario);
    assert!(report.is_pass(), "{}", report.summary());
}

#[test]
fn native_surface_holes_scenario_passes() {
    let scenario = catalog()
        .into_iter()
        .find(|scenario| scenario.name == "render_matrix.native_surface_holes.v1")
        .expect("the native surface hole scenario is registered");
    let report = Runner::from_env().run(scenario);
    assert!(report.is_pass(), "{}", report.summary());
}

#[test]
fn python_surface_holes_scenario_passes() {
    let scenario = catalog()
        .into_iter()
        .find(|scenario| scenario.name == "render_matrix.python_surface_holes.v1")
        .expect("the Python surface hole scenario is registered");
    let report = Runner::from_env().run(scenario);
    assert!(report.is_pass(), "{}", report.summary());
}

#[test]
fn live_coordinate_surface_scenario_passes() {
    let scenario = catalog()
        .into_iter()
        .find(|scenario| scenario.name == "render_matrix.live_coordinate_surface.v1")
        .expect("the live 3D coordinate surface scenario is registered");
    let report = Runner::from_env().run(scenario);
    assert!(report.is_pass(), "{}", report.summary());
}

#[test]
fn live_coordinate_animation_scenario_passes() {
    let scenario = catalog()
        .into_iter()
        .find(|scenario| scenario.name == "render_matrix.live_coordinate_animation.v1")
        .expect("the live coordinate animation scenario is registered");
    let report = Runner::from_env().run(scenario);
    assert!(report.is_pass(), "{}", report.summary());
}

#[test]
fn coordinate_placement_scenario_passes() {
    let scenario = catalog()
        .into_iter()
        .find(|scenario| scenario.name == "render_matrix.coordinate_placement.v1")
        .expect("the coordinate placement scenario is registered");
    let report = Runner::from_env().run(scenario);
    assert!(report.is_pass(), "{}", report.summary());
}

#[test]
fn bundle_render_state_scenario_passes() {
    let scenario = catalog()
        .into_iter()
        .find(|scenario| scenario.name == "lifecycle.bundle_render_state.v1")
        .expect("the bundle render-state scenario is registered");
    let report = Runner::from_env().run(scenario);
    assert!(report.is_pass(), "{}", report.summary());
}

#[test]
fn compact_scene_bundle_scenario_passes() {
    let scenario = catalog()
        .into_iter()
        .find(|scenario| scenario.name == "lifecycle.compact_scene_bundle.v1")
        .expect("the compact scene bundle lifecycle scenario is registered");
    let report = Runner::from_env().run(scenario);
    assert!(report.is_pass(), "{}", report.summary());
}

#[test]
fn install_shared_family_scenario_passes() {
    let scenario = catalog()
        .into_iter()
        .find(|scenario| scenario.name == "lifecycle.install_shared_family.v1")
        .expect("the shared-family install lifecycle scenario is registered");
    let report = Runner::from_env().run(scenario);
    assert!(report.is_pass(), "{}", report.summary());
}

#[test]
fn always_redraw_shared_source_scenario_passes() {
    let scenario = catalog()
        .into_iter()
        .find(|scenario| scenario.name == "lifecycle.always_redraw_shared_source.v1")
        .expect("the shared-source redraw lifecycle scenario is registered");
    let report = Runner::from_env().run(scenario);
    assert!(report.is_pass(), "{}", report.summary());
}

#[test]
fn traced_path_cadence_scenario_passes() {
    let scenario = catalog()
        .into_iter()
        .find(|scenario| scenario.name == "lifecycle.traced_path_cadence.v1")
        .expect("the native traced-path cadence scenario is registered");
    let report = Runner::from_env().run(scenario);
    assert!(report.is_pass(), "{}", report.summary());
}

#[test]
fn owned_tail_branches_scenario_passes() {
    let scenario = catalog()
        .into_iter()
        .find(|scenario| scenario.name == "lifecycle.owned_tail_branches.v1")
        .expect("the owned-tail copy and snapshot scenario is registered");
    let report = Runner::from_env().run(scenario);
    assert!(report.is_pass(), "{}", report.summary());
}

#[test]
fn succession_reverse_seek_scenario_passes() {
    let scenario = catalog()
        .into_iter()
        .find(|scenario| scenario.name == "lifecycle.succession_reverse_seek.v1")
        .expect("the reversible succession render and seek scenario is registered");
    let report = Runner::from_env().run(scenario);
    assert!(report.is_pass(), "{}", report.summary());
}

#[test]
fn tracing_tail_lifecycle_scenario_passes() {
    let scenario = catalog()
        .into_iter()
        .find(|scenario| scenario.name == "lifecycle.tracing_tail_empty_until_update.v1")
        .expect("the tracing-tail lifecycle scenario is registered");
    let report = Runner::from_env().run(scenario);
    assert!(report.is_pass(), "{}", report.summary());
}

#[test]
fn integer_matching_selectors_scenario_passes() {
    let scenario = catalog()
        .into_iter()
        .find(|scenario| scenario.name == "parity.integer_matching_selectors.v1")
        .expect("native integer matching selector scenario is registered");
    let report = Runner::from_env().run(scenario);
    assert!(report.is_pass(), "{}", report.summary());
}

#[test]
fn native_matching_workers_scenario_passes() {
    let scenario = catalog()
        .into_iter()
        .find(|scenario| scenario.name == "render_matrix.native_matching_workers.v1")
        .expect("native matching workers scenario is registered");
    let report = Runner::from_env().run(scenario);
    assert!(report.is_pass(), "{}", report.summary());
}

#[test]
fn metadata_only_capture_scenario_passes() {
    let scenario = catalog()
        .into_iter()
        .find(|scenario| scenario.name == "lifecycle.metadata_only_capture.v1")
        .expect("metadata-only capture scenario is registered");
    let report = Runner::from_env().run(scenario);
    assert!(report.is_pass(), "{}", report.summary());
}

#[test]
fn native_exact_frames_scenario_passes() {
    let scenario = catalog()
        .into_iter()
        .find(|scenario| scenario.name == "render_matrix.native_exact_frames.v1")
        .expect("native exact-frame duration scenario is registered");
    let report = Runner::from_env().run(scenario);
    assert!(report.is_pass(), "{}", report.summary());
}

#[test]
fn native_y4m_conversion_scenario_passes() {
    let scenario = catalog()
        .into_iter()
        .find(|scenario| scenario.name == "render_matrix.native_y4m_conversion.v1")
        .expect("native Y4M conversion scenario is registered");
    let report = Runner::from_env().run(scenario);
    assert!(report.is_pass(), "{}", report.summary());
}

#[test]
fn retained_static_prefix_scenario_passes() {
    let scenario = catalog()
        .into_iter()
        .find(|scenario| scenario.name == "render_matrix.retained_static_prefix.v1")
        .expect("retained static-prefix scenario is registered");
    let report = Runner::from_env().run(scenario);
    assert!(report.is_pass(), "{}", report.summary());
}

#[test]
fn adaptive_static_prefix_scenario_passes() {
    let scenario = catalog()
        .into_iter()
        .find(|scenario| scenario.name == "render_matrix.adaptive_static_prefix.v1")
        .expect("adaptive static-prefix scenario is registered");
    let report = Runner::from_env().run(scenario);
    assert!(report.is_pass(), "{}", report.summary());
}

#[test]
fn obj_concave_faces_scenario_passes() {
    let scenario = catalog()
        .into_iter()
        .find(|scenario| scenario.name == "render_matrix.obj_concave_faces.v1")
        .expect("OBJ concave-face scenario is registered");
    let report = Runner::from_env().run(scenario);
    assert!(report.is_pass(), "{}", report.summary());
}

#[test]
fn camera_projection_resources_scenario_passes() {
    let scenario = catalog()
        .into_iter()
        .find(|scenario| scenario.name == "render_matrix.camera_projection_resources.v1")
        .expect("camera projection-resource scenario is registered");
    let report = Runner::from_env().run(scenario);
    assert!(report.is_pass(), "{}", report.summary());
}

#[test]
fn camera_world_resources_scenario_passes() {
    let scenario = catalog()
        .into_iter()
        .find(|scenario| scenario.name == "render_matrix.camera_world_resources.v1")
        .expect("camera world-resource scenario is registered");
    let report = Runner::from_env().run(scenario);
    assert!(report.is_pass(), "{}", report.summary());
}

#[test]
fn camera_tile_reuse_scenario_passes() {
    let scenario = catalog()
        .into_iter()
        .find(|scenario| scenario.name == "render_matrix.camera_tile_reuse.v1")
        .expect("camera tile reuse scenario is registered");
    let report = Runner::from_env().run(scenario);
    assert!(report.is_pass(), "{}", report.summary());
}

#[test]
fn obj_texture_options_scenario_passes() {
    let scenario = catalog()
        .into_iter()
        .find(|scenario| scenario.name == "render_matrix.obj_texture_options.v1")
        .expect("OBJ texture-options scenario is registered");
    let report = Runner::from_env().run(scenario);
    assert!(report.is_pass(), "{}", report.summary());
}

#[test]
fn obj_smoothing_groups_scenario_passes() {
    let scenario = catalog()
        .into_iter()
        .find(|scenario| scenario.name == "render_matrix.obj_smoothing_groups.v1")
        .expect("OBJ smoothing-groups scenario is registered");
    let report = Runner::from_env().run(scenario);
    assert!(report.is_pass(), "{}", report.summary());
}

#[test]
fn snapshot_handle_safety_scenario_passes() {
    let scenario = catalog()
        .into_iter()
        .find(|scenario| scenario.name == "lifecycle.snapshot_handle_safety.v1")
        .expect("snapshot handle safety scenario is registered");
    let report = Runner::from_env().run(scenario);
    assert!(report.is_pass(), "{}", report.summary());
}

#[test]
fn native_positional_lifecycle_scenario_passes() {
    let scenario = catalog()
        .into_iter()
        .find(|scenario| scenario.name == "lifecycle.native_positional_wag.v1")
        .expect("native positional lifecycle scenario is registered");
    let report = Runner::from_env().run(scenario);
    assert!(report.is_pass(), "{}", report.summary());
}

#[test]
fn native_final_png_scenario_passes() {
    let scenario = catalog()
        .into_iter()
        .find(|scenario| scenario.name == "render_matrix.native_final_png.v1")
        .expect("native final PNG scenario is registered");
    let report = Runner::from_env().run(scenario);
    assert!(report.is_pass(), "{}", report.summary());
}

#[test]
fn native_shape_matching_scenario_passes() {
    let scenario = catalog()
        .into_iter()
        .find(|scenario| scenario.name == "render_matrix.native_shape_matching.v1")
        .expect("native shape matching scenario is registered");
    let report = Runner::from_env().run(scenario);
    assert!(report.is_pass(), "{}", report.summary());
}

#[test]
fn prismify_compound_cap_scenario_passes() {
    let scenario = catalog()
        .into_iter()
        .find(|scenario| scenario.name == "render_matrix.prismify_compound_cap.v1")
        .expect("native compound cap scenario is registered");
    let report = Runner::from_env().run(scenario);
    assert!(report.is_pass(), "{}", report.summary());
}

/// Report actual native/compressed artifact coverage without inventing optional tool runs.
#[test]
fn python_bundle_audio_capabilities_scenario_passes() {
    let scenario = catalog()
        .into_iter()
        .find(|scenario| scenario.name == "lifecycle.python_bundle_audio_capabilities.v1")
        .expect("Python bundle audio capabilities are registered");
    let report = Runner::from_env().run(scenario);
    assert!(report.is_pass(), "{}", report.summary());
}

#[test]
fn python_text_configuration_scenario_passes() {
    let scenario = catalog()
        .into_iter()
        .find(|scenario| scenario.name == "lifecycle.python_text_configuration.v1")
        .expect("native Python text configuration is registered");
    let report = Runner::from_env().run(scenario);
    assert!(report.is_pass(), "{}", report.summary());
}

#[test]
fn bundle_soundtrack_capabilities_scenario_passes() {
    let scenario = catalog()
        .into_iter()
        .find(|scenario| scenario.name == "lifecycle.bundle_soundtrack_capabilities.v1")
        .expect("bundle soundtrack capabilities are registered");
    let report = Runner::from_env().run(scenario);
    assert!(report.is_pass(), "{}", report.summary());
}

#[test]
fn native_soundtrack_wav_scenario_passes() {
    let scenario = catalog()
        .into_iter()
        .find(|scenario| scenario.name == "render_matrix.native_soundtrack_wav.v1")
        .expect("native soundtrack WAV scenario is registered");
    let report = Runner::from_env().run(scenario);
    assert!(report.is_pass(), "{}", report.summary());
}

#[test]
fn piano_keyboard_scenario_passes() {
    let scenario = catalog()
        .into_iter()
        .find(|scenario| scenario.name == "drawings.piano_keyboard.v1")
        .expect("native piano keyboard scenario is registered");
    let report = Runner::from_env().run(scenario);
    assert!(report.is_pass(), "{}", report.summary());
}

/// fm-5wq.46's native semantic-oracle scenario, focused.
#[test]
fn semantic_oracles_native_scenario_passes() {
    let scenario = catalog()
        .into_iter()
        .find(|scenario| scenario.name == "render_matrix.semantic_oracles.native.v1")
        .expect("the native semantic-oracle scenario is registered");
    let report = Runner::from_env().run(scenario);
    assert!(report.is_pass(), "{}", report.summary());
}

/// fm-5wq.46's portal semantic-oracle scenario, focused.
#[test]
fn semantic_oracles_portal_scenario_passes() {
    let scenario = catalog()
        .into_iter()
        .find(|scenario| scenario.name == "render_matrix.semantic_oracles.portal.v1")
        .expect("the portal semantic-oracle scenario is registered");
    let report = Runner::from_env().run(scenario);
    assert!(report.is_pass(), "{}", report.summary());
}

/// Focused acceptance for the Python production-composition seam. The same
/// spec also remains in the per-commit fast-tier catalog.
#[test]
fn python_portal_png_scenario_passes() {
    let scenario = catalog()
        .into_iter()
        .find(|scenario| scenario.name == "render_matrix.python_portal_png_sequence.v1")
        .expect("Python portal scenario is registered");
    let report = Runner::from_env().run(scenario);
    assert!(report.is_pass(), "{}", report.summary());
}

/// The production Python capture and native read-only worker in the fast tier.
#[test]
fn python_studio_capture_scenario_passes() {
    let scenario = catalog()
        .into_iter()
        .find(|scenario| scenario.name == "lifecycle.python_studio_capture.v1")
        .expect("Python Studio capture is registered");
    let report = Runner::from_env().run(scenario);
    assert!(report.is_pass(), "{}", report.summary());
}

/// Native InteractiveScene gesture modifiers, key repeats and undo history.
#[test]
fn python_studio_editing_scenario_passes() {
    let scenario = catalog()
        .into_iter()
        .find(|scenario| scenario.name == "lifecycle.python_studio_editing.v1")
        .expect("Python Studio editing is registered");
    let report = Runner::from_env().run(scenario);
    assert!(report.is_pass(), "{}", report.summary());
}

/// Native numbered inserts, ownership, cancellation and atomic publication.
#[test]
fn python_insert_recording_scenario_passes() {
    let scenario = catalog()
        .into_iter()
        .find(|scenario| scenario.name == "lifecycle.python_insert_recording.v1")
        .expect("Python insert recording is registered");
    let report = Runner::from_env().run(scenario);
    assert!(report.is_pass(), "{}", report.summary());
}

/// Authored streamline colors reach native frames and retain atomic restyling.
#[test]
fn python_streamline_colors_scenario_passes() {
    let scenario = catalog()
        .into_iter()
        .find(|scenario| scenario.name == "render_matrix.python_streamline_colors.v1")
        .expect("Python streamline colors are registered");
    let report = Runner::from_env().run(scenario);
    assert!(report.is_pass(), "{}", report.summary());
}

/// Time-varying field copies and targets sample their own clocks.
#[test]
fn python_time_varying_copies_scenario_passes() {
    let scenario = catalog()
        .into_iter()
        .find(|scenario| scenario.name == "lifecycle.python_time_varying_copies.v1")
        .expect("Python time-varying copies are registered");
    let report = Runner::from_env().run(scenario);
    assert!(report.is_pass(), "{}", report.summary());
}

#[test]
fn family_reversal_contour_scenario_passes() {
    let scenario = catalog()
        .into_iter()
        .find(|scenario| scenario.name == "lifecycle.family_reversal_end_handle.v1")
        .expect("family reversal is registered");
    let report = Runner::from_env().run(scenario);
    assert!(report.is_pass(), "{}", report.summary());
}

#[cfg(unix)]
#[test]
fn encoder_help_process_evidence_scenario_passes() {
    let scenario = catalog()
        .into_iter()
        .find(|scenario| scenario.name == "failure_path.encoder_help_process_evidence.v1")
        .expect("encoder help process evidence is registered");
    let report = Runner::from_env().run(scenario);
    assert!(report.is_pass(), "{}", report.summary());
}

#[test]
fn tex_ams_scenario_passes() {
    let scenario = catalog()
        .into_iter()
        .find(|scenario| scenario.name == "parity.tex_ams.v1")
        .expect("AMS native rendering is registered");
    let report = Runner::from_env().run(scenario);
    assert!(report.is_pass(), "{}", report.summary());
}

/// fm-longform-native-e2e-hoeffding-l59d: the explainer's portal segments,
/// focused.
#[test]
fn hoeffding_d_portal_scenario_passes() {
    let scenario = catalog()
        .into_iter()
        .find(|scenario| scenario.name == "render_matrix.hoeffding_d_portal.v1")
        .expect("the Hoeffding's D portal scenario is registered");
    let report = Runner::from_env().run(scenario);
    assert!(report.is_pass(), "{}", report.summary());
}

/// fm-longform-native-e2e-hoeffding-l59d: the whole explainer at 1080p60 with
/// sound, focused: green under `FMN_E2E_FULL=1` (run it with `--release`), a
/// SKIP otherwise. Its chapters' fast row runs once, in the fast tier.
#[test]
fn hoeffding_d_full_scenario_passes_when_enabled() {
    let scenario = catalog()
        .into_iter()
        .find(|scenario| scenario.name == "render_matrix.hoeffding_d_full_mp4.v1")
        .expect("the Hoeffding's D full scenario is registered");
    let report = Runner::from_env().run(scenario);
    if e2e::full_matrix_from_env() {
        assert!(report.is_pass(), "{}", report.summary());
    } else {
        assert!(
            matches!(report.status, Status::Skipped(_)),
            "{}",
            report.summary()
        );
    }
}

/// Quality flag interactions and encoder compatibility fail before tool discovery.
#[test]
fn cli_video_quality_admission_scenario_passes() {
    let scenario = catalog()
        .into_iter()
        .find(|scenario| scenario.name == "failure_path.cli_video_quality_admission.v1")
        .expect("CLI video-quality scenario is registered");
    let report = Runner::from_env().run(scenario);
    assert!(report.is_pass(), "{}", report.summary());
}

/// CLI ffmpeg budgets share native admission and refuse before tool discovery.
#[test]
fn cli_ffmpeg_limit_admission_scenario_passes() {
    let scenario = catalog()
        .into_iter()
        .find(|scenario| scenario.name == "failure_path.cli_ffmpeg_limit_admission.v1")
        .expect("CLI ffmpeg-limit scenario is registered");
    let report = Runner::from_env().run(scenario);
    assert!(report.is_pass(), "{}", report.summary());
}

/// The facade's video formats need an explicit ffmpeg capability (D2).
#[test]
fn facade_video_capability_scenario_passes() {
    let scenario = catalog()
        .into_iter()
        .find(|scenario| scenario.name == "failure_path.facade_video_requires_ffmpeg_capability.v1")
        .expect("facade video capability scenario is registered");
    let report = Runner::from_env().run(scenario);
    assert!(report.is_pass(), "{}", report.summary());
}

/// fm-cli-flag-timing-ht01: window flags refuse file renders; timing holds.
#[test]
fn cli_window_flag_scenario_passes() {
    let scenario = catalog()
        .into_iter()
        .find(|scenario| scenario.name == "failure_path.cli_window_flag_refused_timing_kept.v1")
        .expect("CLI window-flag scenario is registered");
    let report = Runner::from_env().run(scenario);
    assert!(report.is_pass(), "{}", report.summary());
}

/// Portal admission must produce real native jobs and preserve serial output.
#[test]
fn python_native_offload_scenario_passes() {
    let scenario = catalog()
        .into_iter()
        .find(|scenario| scenario.name == "render_matrix.python_portal_native_offload.v1")
        .expect("Python native offload scenario is registered");
    let report = Runner::from_env().run(scenario);
    assert!(report.is_pass(), "{}", report.summary());
}

#[test]
fn python_exact_rate_offload_scenario_passes() {
    let scenario = catalog()
        .into_iter()
        .find(|scenario| scenario.name == "lifecycle.rate_func_tabulated_offload.v1")
        .expect("exact Python rate offload scenario is registered");
    let report = Runner::from_env().run(scenario);
    assert!(report.is_pass(), "{}", report.summary());
}

/// fm-sq8.11: the portal's planar route, certified parity with native fmn.
#[test]
fn python_planar_route_scenario_passes() {
    let scenario = catalog()
        .into_iter()
        .find(|scenario| scenario.name == "render_matrix.python_portal_planar_route.v1")
        .expect("Python planar route scenario is registered");
    let report = Runner::from_env().run(scenario);
    assert!(report.is_pass(), "{}", report.summary());
}

#[test]
fn python_planar_aspects_scenario_passes() {
    let scenario = catalog()
        .into_iter()
        .find(|scenario| scenario.name == "render_matrix.python_portal_planar_aspects.v1")
        .expect("non-widescreen Python planar routes are registered");
    let report = Runner::from_env().run(scenario);
    assert!(report.is_pass(), "{}", report.summary());
}

/// Portal scenes default to the Reference's 30 fps on #333333; braces span
/// detached composites (fm-5wq.44, fm-5wq.43).
#[test]
fn python_reference_defaults_scenario_passes() {
    let scenario = catalog()
        .into_iter()
        .find(|scenario| scenario.name == "parity.python_portal_reference_defaults.v1")
        .expect("Python reference defaults scenario is registered");
    let report = Runner::from_env().run(scenario);
    assert!(report.is_pass(), "{}", report.summary());
}

#[test]
fn python_reference_render_flags_scenario_passes() {
    let scenario = catalog()
        .into_iter()
        .find(|scenario| scenario.name == "lifecycle.python_reference_render_flags.v1")
        .expect("Python Reference render flags scenario is registered");
    let report = Runner::from_env().run(scenario);
    assert!(report.is_pass(), "{}", report.summary());
}

/// Decoded native GIF/y4m/WAV, ordering, cue placement, and failure atomicity.
#[test]
fn python_native_outputs_scenario_passes() {
    let scenario = catalog()
        .into_iter()
        .find(|scenario| scenario.name == "render_matrix.python_portal_native_outputs.v1")
        .expect("Python native output scenario is registered");
    let report = Runner::from_env().run(scenario);
    assert!(report.is_pass(), "{}", report.summary());
}

/// Grouped field motion keeps placement and reaches native rendered output.
#[test]
fn native_field_advection_scenario_passes() {
    let scenario = catalog()
        .into_iter()
        .find(|scenario| scenario.name == "render_matrix.native_field_advection.v1")
        .expect("native field advection is registered");
    let report = Runner::from_env().run(scenario);
    assert!(report.is_pass(), "{}", report.summary());
}

#[test]
fn native_conditional_waits_scenario_passes() {
    let scenario = catalog()
        .into_iter()
        .find(|scenario| scenario.name == "render_matrix.native_conditional_waits.v1")
        .expect("native conditional wait scenario is registered");
    let report = Runner::from_env().run(scenario);
    assert!(report.is_pass(), "{}", report.summary());
}

#[test]
fn native_detached_areas_scenario_passes() {
    let scenario = catalog()
        .into_iter()
        .find(|scenario| scenario.name == "render_matrix.native_detached_areas.v1")
        .expect("native detached area scenario is registered");
    let report = Runner::from_env().run(scenario);
    assert!(report.is_pass(), "{}", report.summary());
}

#[test]
fn native_detached_calculus_scenario_passes() {
    let scenario = catalog()
        .into_iter()
        .find(|scenario| scenario.name == "render_matrix.native_detached_calculus.v1")
        .expect("native detached calculus scenario is registered");
    let report = Runner::from_env().run(scenario);
    assert!(report.is_pass(), "{}", report.summary());
}

#[test]
fn transparent_video_encoder_scenario_passes() {
    let scenario = catalog()
        .into_iter()
        .find(|scenario| scenario.name == "failure_path.transparent_video_encoder.v1")
        .expect("transparent video encoder scenario is registered");
    let report = Runner::from_env().run(scenario);
    assert!(report.is_pass(), "{}", report.summary());
}

#[test]
fn native_tex_constructors_scenario_passes() {
    let scenario = catalog()
        .into_iter()
        .find(|scenario| scenario.name == "render_matrix.native_tex_constructors.v1")
        .expect("native TeX constructor scenario is registered");
    let report = Runner::from_env().run(scenario);
    assert!(report.is_pass(), "{}", report.summary());
}

#[test]
fn prelude_families_scenario_passes() {
    let scenario = catalog()
        .into_iter()
        .find(|scenario| scenario.name == "render_matrix.prelude_families.v1")
        .expect("native prelude family scenario is registered");
    let report = Runner::from_env().run(scenario);
    assert!(report.is_pass(), "{}", report.summary());
}

#[test]
fn markdown_inline_math_scenario_passes() {
    let scenario = catalog()
        .into_iter()
        .find(|scenario| scenario.name == "parity.markdown_inline_math.v1")
        .expect("Markdown inline-math scenario is registered");
    let report = Runner::from_env().run(scenario);
    assert!(report.is_pass(), "{}", report.summary());
}

/// Native live-number layout, animation, thread replay and failed publication.
#[test]
fn python_live_tex_scenario_passes() {
    let scenario = catalog()
        .into_iter()
        .find(|scenario| scenario.name == "render_matrix.python_live_tex.v1")
        .expect("Python live equation scenario is registered");
    let report = Runner::from_env().run(scenario);
    assert!(report.is_pass(), "{}", report.summary());
}

/// Native complex-number layout, animation, thread replay and failed publication.
#[test]
fn python_complex_readouts_scenario_passes() {
    let scenario = catalog()
        .into_iter()
        .find(|scenario| scenario.name == "render_matrix.python_complex_readouts.v1")
        .expect("Python complex readout scenario is registered");
    let report = Runner::from_env().run(scenario);
    assert!(report.is_pass(), "{}", report.summary());
}

/// Native macro layout, matching, source spans and failed publication.
#[test]
fn python_tex_preamble_scenario_passes() {
    let scenario = catalog()
        .into_iter()
        .find(|scenario| scenario.name == "render_matrix.python_tex_preamble.v1")
        .expect("Python TeX preamble scenario is registered");
    let report = Runner::from_env().run(scenario);
    assert!(report.is_pass(), "{}", report.summary());
}

#[test]
fn python_subclass_hooks_scenario_passes() {
    let scenario = catalog()
        .into_iter()
        .find(|scenario| scenario.name == "lifecycle.python_subclass_hooks.v1")
        .expect("library subclass hook scenario is registered");
    let report = Runner::from_env().run(scenario);
    assert!(report.is_pass(), "{}", report.summary());
}

#[test]
fn python_witness_registry_scenario_passes() {
    let scenario = catalog()
        .into_iter()
        .find(|scenario| scenario.name == "failure_path.python_witness_registry.v1")
        .expect("witness registry scenario is registered");
    let report = Runner::from_env().run(scenario);
    assert!(report.is_pass(), "{}", report.summary());
}

/// Sampled wireframes through the native portal, renderer and PNG publisher.
#[test]
fn python_svg_scenario_passes() {
    let scenario = catalog()
        .into_iter()
        .find(|scenario| scenario.name == "render_matrix.python_svg.v1")
        .expect("Python SVG scenario is registered");
    let report = Runner::from_env().run(scenario);
    assert!(report.is_pass(), "{}", report.summary());
}

#[test]
fn python_textured_surfaces_scenario_passes() {
    let scenario = catalog()
        .into_iter()
        .find(|scenario| scenario.name == "render_matrix.python_textured_surfaces.v1")
        .expect("textured surface scenario is registered");
    let report = Runner::from_env().run(scenario);
    assert!(report.is_pass(), "{}", report.summary());
}

#[test]
fn python_image_inputs_scenario_passes() {
    let scenario = catalog()
        .into_iter()
        .find(|scenario| scenario.name == "lifecycle.python_image_inputs.v1")
        .expect("Python image input lifecycle is registered");
    let report = Runner::from_env().run(scenario);
    assert!(report.is_pass(), "{}", report.summary());
}

#[test]
fn geometry_defaults_both_doors_scenario_passes() {
    let scenario = catalog()
        .into_iter()
        .find(|scenario| scenario.name == "parity.geometry_defaults_both_doors.v1")
        .expect("both-doors geometry defaults scenario is registered");
    let report = Runner::from_env().run(scenario);
    assert!(report.is_pass(), "{}", report.summary());
}

#[test]
fn portal_asset_root_scenario_passes() {
    let scenario = catalog()
        .into_iter()
        .find(|scenario| scenario.name == "lifecycle.portal_asset_root.v1")
        .expect("portal asset-root lifecycle is registered");
    let report = Runner::from_env().run(scenario);
    assert!(report.is_pass(), "{}", report.summary());
}

#[test]
fn python_surface_mesh_scenario_passes() {
    let scenario = catalog()
        .into_iter()
        .find(|scenario| scenario.name == "render_matrix.python_surface_mesh.v1")
        .expect("Python surface mesh scenario is registered");
    let report = Runner::from_env().run(scenario);
    assert!(report.is_pass(), "{}", report.summary());
}

/// Native adaptive trajectories, arbitrary dimensions, and callback failures.
#[test]
fn python_ode_solution_scenario_passes() {
    let scenario = catalog()
        .into_iter()
        .find(|scenario| scenario.name == "parity.python_ode_solution.v1")
        .expect("Python ODE solution scenario is registered");
    let report = Runner::from_env().run(scenario);
    assert!(report.is_pass(), "{}", report.summary());
}

/// Curved transforms, authored hooks and composition through real PNG output.
#[test]
fn python_animation_lifecycle_scenario_passes() {
    let scenario = catalog()
        .into_iter()
        .find(|scenario| scenario.name == "lifecycle.python_animation_semantics.v1")
        .expect("Python animation lifecycle scenario is registered");
    let report = Runner::from_env().run(scenario);
    assert!(report.is_pass(), "{}", report.summary());
}

/// Focused acceptance for the portal's one-frame final-state route, including
/// the no-clobber planted negative.
#[test]
fn python_portal_png_still_scenario_passes() {
    let scenario = catalog()
        .into_iter()
        .find(|scenario| scenario.name == "render_matrix.python_portal_png_still.v1")
        .expect("Python portal still scenario is registered");
    let report = Runner::from_env().run(scenario);
    assert!(report.is_pass(), "{}", report.summary());
}

#[test]
fn retained_path_motion_scenario_passes() {
    let scenario = catalog()
        .into_iter()
        .find(|scenario| scenario.name == "parity.retained_path_motion.v1")
        .expect("retained path motion scenario is registered");
    let report = Runner::from_env().run(scenario);
    assert!(report.is_pass(), "{}", report.summary());
}

#[test]
fn owned_path_observation_scenario_passes() {
    let scenario = catalog()
        .into_iter()
        .find(|scenario| scenario.name == "lifecycle.owned_path_observations.v1")
        .expect("owned path observation scenario is registered");
    let report = Runner::from_env().run(scenario);
    assert!(report.is_pass(), "{}", report.summary());
}

#[test]
fn move_along_path_live_input_failure_scenario_passes() {
    let scenario = catalog()
        .into_iter()
        .find(|scenario| scenario.name == "failure_path.move_along_path_live_input.v1")
        .expect("motion-path failure scenario is registered");
    let report = Runner::from_env().run(scenario);
    assert!(report.is_pass(), "{}", report.summary());
}

/// Focused entry for the same mandatory fast-tier production lifecycle row.
#[test]
fn studio_compiled_preview_scenario_passes() {
    let scenario = catalog()
        .into_iter()
        .find(|scenario| scenario.name == "lifecycle.studio_compiled_preview.v1")
        .expect("compiled Studio preview scenario is registered");
    let report = Runner::from_env().run(scenario);
    assert!(report.is_pass(), "{}", report.summary());
}

#[test]
fn studio_camera_preview_scenario_passes() {
    let scenario = catalog()
        .into_iter()
        .find(|scenario| scenario.name == "render_matrix.studio_camera_preview.v1")
        .expect("the live Studio camera scenario is registered");
    let report = Runner::from_env().run(scenario);
    assert!(report.is_pass(), "{}", report.summary());
}

#[test]
fn studio_pure_checkpoint_restore_scenario_passes() {
    let scenario = catalog()
        .into_iter()
        .find(|scenario| scenario.name == "lifecycle.studio_checkpoint_restore.v1")
        .expect("native pure checkpoint scenario is registered");
    let report = Runner::from_env().run(scenario);
    assert!(report.is_pass(), "{}", report.summary());
}

#[test]
fn studio_mjpeg_preview_scenario_passes() {
    let scenario = catalog()
        .into_iter()
        .find(|scenario| scenario.name == "lifecycle.studio_mjpeg_preview.v1")
        .expect("MJPEG preview scenario is registered");
    let report = Runner::from_env().run(scenario);
    assert!(report.is_pass(), "{}", report.summary());
}

#[test]
fn studio_terminal_controls_scenario_passes() {
    let scenario = catalog()
        .into_iter()
        .find(|scenario| scenario.name == "lifecycle.studio_terminal_controls.v1")
        .expect("terminal Studio controls scenario is registered");
    let report = Runner::from_env().run(scenario);
    assert!(report.is_pass(), "{}", report.summary());
}

#[test]
fn studio_registered_checkpoint_scenario_passes() {
    let scenario = catalog()
        .into_iter()
        .find(|scenario| scenario.name == "lifecycle.studio_registered_checkpoint.v1")
        .expect("registered native checkpoint scenario is registered");
    let report = Runner::from_env().run(scenario);
    assert!(report.is_pass(), "{}", report.summary());
}

#[test]
fn studio_wait_tail_checkpoint_scenario_passes() {
    let scenario = catalog()
        .into_iter()
        .find(|scenario| scenario.name == "lifecycle.studio_wait_tail_checkpoint.v1")
        .expect("completed native prefix checkpoint scenario is registered");
    let report = Runner::from_env().run(scenario);
    assert!(report.is_pass(), "{}", report.summary());
}

#[test]
fn studio_native_worker_lifecycle_scenario_passes() {
    let scenario = catalog()
        .into_iter()
        .find(|scenario| scenario.name == "lifecycle.studio_native_worker.v1")
        .expect("native Studio lifecycle scenario is registered");
    let report = Runner::from_env().run(scenario);
    assert!(report.is_pass(), "{}", report.summary());
}

#[test]
fn studio_native_interactive_lifecycle_scenario_passes() {
    let scenario = catalog()
        .into_iter()
        .find(|scenario| scenario.name == "lifecycle.studio_native_interactive.v1")
        .expect("native Studio interactive lifecycle scenario is registered");
    let report = Runner::from_env().run(scenario);
    assert!(report.is_pass(), "{}", report.summary());
}

#[test]
fn studio_presenter_holds_scenario_passes() {
    let scenario = catalog()
        .into_iter()
        .find(|scenario| scenario.name == "lifecycle.studio_presenter_holds.v1")
        .expect("native Studio presenter scenario is registered");
    let report = Runner::from_env().run(scenario);
    assert!(report.is_pass(), "{}", report.summary());
}

#[test]
fn python_point_cloud_uniforms_scenario_passes() {
    let scenario = catalog()
        .into_iter()
        .find(|scenario| scenario.name == "lifecycle.python_point_cloud_uniforms.v1")
        .expect("point-cloud uniform lifecycle scenario is registered");
    let report = Runner::from_env().run(scenario);
    assert!(report.is_pass(), "{}", report.summary());
}

#[test]
fn studio_session_capability_isolation_scenario_passes() {
    let scenario = catalog()
        .into_iter()
        .find(|scenario| scenario.name == "lifecycle.studio_session_capability_isolation.v1")
        .expect("native session capability isolation scenario is registered");
    let report = Runner::from_env().run(scenario);
    assert!(report.is_pass(), "{}", report.summary());
}

#[test]
fn studio_native_builtin_input_lifecycle_scenario_passes() {
    let scenario = catalog()
        .into_iter()
        .find(|scenario| scenario.name == "lifecycle.studio_builtin_inputs.v1")
        .expect("native builtin input lifecycle scenario is registered");
    let report = Runner::from_env().run(scenario);
    assert!(report.is_pass(), "{}", report.summary());
}

#[test]
fn cli_certification_admission_scenario_passes() {
    let scenario = catalog()
        .into_iter()
        .find(|scenario| scenario.name == "lifecycle.cli_certification_admission.v1")
        .expect("native certification admission scenario is registered");
    let report = Runner::from_env().run(scenario);
    assert!(report.is_pass(), "{}", report.summary());
}

#[test]
fn cli_doctor_capabilities_scenario_passes() {
    let scenario = catalog()
        .into_iter()
        .find(|scenario| scenario.name == "lifecycle.cli_doctor_capabilities.v1")
        .expect("doctor capability scenario is registered");
    let report = Runner::from_env().run(scenario);
    assert!(report.is_pass(), "{}", report.summary());
}

#[test]
fn live_bar_chart_scenario_passes() {
    let scenario = catalog()
        .into_iter()
        .find(|scenario| scenario.name == "lifecycle.live_bar_chart.v1")
        .expect("live native bar chart scenario is registered");
    let report = Runner::from_env().run(scenario);
    assert!(report.is_pass(), "{}", report.summary());
}

#[test]
fn native_prepared_images_scenario_passes() {
    let scenario = catalog()
        .into_iter()
        .find(|scenario| scenario.name == "lifecycle.native_prepared_images.v1")
        .expect("native prepared image scenario is registered");
    let report = Runner::from_env().run(scenario);
    assert!(report.is_pass(), "{}", report.summary());
}

#[test]
fn python_graph_animations_scenario_passes() {
    let scenario = catalog()
        .into_iter()
        .find(|scenario| scenario.name == "lifecycle.python_graph_animations.v1")
        .expect("Python graph animation scenario is registered");
    let report = Runner::from_env().run(scenario);
    assert!(report.is_pass(), "{}", report.summary());
}

#[test]
fn native_method_override_scenario_passes() {
    let scenario = catalog()
        .into_iter()
        .find(|scenario| scenario.name == "lifecycle.native_method_override.v1")
        .expect("native method override scenario is registered");
    let report = Runner::from_env().run(scenario);
    assert!(report.is_pass(), "{}", report.summary());
}

#[test]
fn graph_animations_scenario_passes() {
    let scenario = catalog()
        .into_iter()
        .find(|scenario| scenario.name == "lifecycle.graph_animations.v1")
        .expect("native graph animation lifecycle is registered");
    let report = Runner::from_env().run(scenario);
    assert!(report.is_pass(), "{}", report.summary());
}

#[test]
fn native_pure_camera_scenario_passes() {
    let scenario = catalog()
        .into_iter()
        .find(|scenario| scenario.name == "lifecycle.native_pure_camera.v1")
        .expect("native pure camera scenario is registered");
    let report = Runner::from_env().run(scenario);
    assert!(report.is_pass(), "{}", report.summary());
}

#[test]
fn python_control_panel_frame_scenario_passes() {
    let scenario = catalog()
        .into_iter()
        .find(|scenario| scenario.name == "lifecycle.python_control_panel_frame.v1")
        .expect("frame-aware Python control panel scenario is registered");
    let report = Runner::from_env().run(scenario);
    assert!(report.is_pass(), "{}", report.summary());
}

#[test]
fn neural_network_live_values_scenario_passes() {
    let scenario = catalog()
        .into_iter()
        .find(|scenario| scenario.name == "neural_network.live_training_and_purity.v1")
        .expect("native neural live-value lifecycle is registered");
    let report = Runner::from_env().run(scenario);
    assert!(report.is_pass(), "{}", report.summary());
}

#[test]
fn partial_reveal_contours_scenario_passes() {
    let scenario = catalog()
        .into_iter()
        .find(|scenario| scenario.name == "lifecycle.partial_reveal_contours.v1")
        .expect("partial-reveal contour lifecycle is registered");
    let report = Runner::from_env().run(scenario);
    assert!(report.is_pass(), "{}", report.summary());
}

#[test]
fn studio_drawing_checkpoint_scenario_passes() {
    let scenario = catalog()
        .into_iter()
        .find(|scenario| scenario.name == "lifecycle.studio_drawing_checkpoint.v1")
        .expect("drawing checkpoint lifecycle is registered");
    let report = Runner::from_env().run(scenario);
    assert!(report.is_pass(), "{}", report.summary());
}

#[test]
fn studio_group_checkpoint_scenario_passes() {
    let scenario = catalog()
        .into_iter()
        .find(|scenario| scenario.name == "lifecycle.studio_group_checkpoint.v1")
        .expect("group checkpoint lifecycle is registered");
    let report = Runner::from_env().run(scenario);
    assert!(report.is_pass(), "{}", report.summary());
}

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

/// The Parity Ledger ruling drills' stable scenario ids: one per ruling
/// family (C-5, C-17, C-15, C-16, BN-03, and the §11.3 span map). The
/// invariant pins each id so a ruling can never silently lose its e2e
/// drill.
const PARITY_RULING_DRILL_IDS: &[&str] = &[
    "parity.updater_call_runs_once.v1",
    "parity.plane_y_unit_size.v1",
    "parity.function_graph_color.v1",
    "parity.frame_markers_to_edge.v1",
    "parity.move_along_path_true_arclength.v1",
    "parity.tex_span_map_t2c.v1",
    "parity.tex_cm_recipes.v1",
];

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

    // ParityDrill is the Parity Ledger's ruling class: never empty again,
    // and every ruling family keeps its stable drill id.
    let parity_seeded = scenarios
        .iter()
        .filter(|s| s.class == ScenarioClass::ParityDrill && s.regression.is_none())
        .count();
    assert!(
        parity_seeded >= 1,
        "ParityDrill has {parity_seeded} scenarios"
    );
    for id in PARITY_RULING_DRILL_IDS {
        assert!(
            scenarios.iter().any(|scenario| scenario.name == *id),
            "ParityDrill lost its stable ruling drill {id}"
        );
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
    assert!(
        scenarios.iter().any(
            |scenario| scenario.name == "lifecycle.studio_native_worker.v1"
                && scenario.surface == Surface::StudioSubprocess
                && scenario.tier == Tier::Fast
                && scenario.regression.is_none()
        ),
        "Studio lost its mandatory native subprocess lifecycle scenario"
    );
    assert!(
        scenarios
            .iter()
            .any(|scenario| scenario.surface == Surface::PythonInProcess),
        "Python lost its production composition scenario"
    );
}

#[test]
fn python_class_sweep_scenario_passes() {
    let scenario = catalog()
        .into_iter()
        .find(|scenario| scenario.name == "parity.class_sweep.v1")
        .expect("class sweep scenario is registered");
    let report = Runner::from_env().run(scenario);
    assert!(report.is_pass(), "{}", report.summary());
}

#[test]
fn python_structural_envelope_scenario_passes() {
    let scenario = catalog()
        .into_iter()
        .find(|scenario| scenario.name == "parity.structural_envelope.v1")
        .expect("structural envelope scenario is registered");
    let report = Runner::from_env().run(scenario);
    assert!(report.is_pass(), "{}", report.summary());
}

#[test]
fn python_reference_corpus_regressions_scenario_passes() {
    let scenario = catalog()
        .into_iter()
        .find(|scenario| scenario.name == "parity.reference_corpus_regressions.v1")
        .expect("Reference corpus regressions scenario is registered");
    let report = Runner::from_env().run(scenario);
    assert!(report.is_pass(), "{}", report.summary());
}

#[test]
fn python_tex_selection_scenario_passes() {
    let scenario = catalog()
        .into_iter()
        .find(|scenario| scenario.name == "parity.tex_selection_reference.v1")
        .expect("Tex selection scenario is registered");
    let report = Runner::from_env().run(scenario);
    assert!(report.is_pass(), "{}", report.summary());
}

/// fm-5wq.34: the differential fuzz smoke runs where the pinned Reference and a
/// portal wheel are installed, and anywhere else reports that it did not run,
/// which is never a pass. The full-matrix gate is forced on so the surface's own
/// check decides.
#[test]
fn differential_fuzz_smoke_runs_or_reports_not_run() {
    let scenario = catalog()
        .into_iter()
        .find(|scenario| scenario.name == "parity.differential_fuzz_smoke.v1")
        .expect("differential fuzz smoke is registered");
    let available = Surface::ReferenceDifferential
        .unavailable_reason()
        .is_none();
    let report = Runner::from_env().run_gated(scenario, true);
    if available {
        assert!(report.is_pass(), "{}", report.summary());
    } else {
        assert!(
            matches!(report.status, Status::Skipped(reason)
                if reason.starts_with("not-run: reference-env-absent")),
            "{}",
            report.summary()
        );
    }
}

#[test]
fn python_structural_facts_scenario_passes() {
    let scenario = catalog()
        .into_iter()
        .find(|scenario| scenario.name == "parity.structural_facts.v1")
        .expect("structural facts scenario is registered");
    let report = Runner::from_env().run(scenario);
    assert!(report.is_pass(), "{}", report.summary());
}

#[test]
fn python_surface_hooks_scenario_passes() {
    let scenario = catalog()
        .into_iter()
        .find(|scenario| scenario.name == "lifecycle.python_surface_hooks.v1")
        .expect("Python surface hooks scenario is registered");
    let report = Runner::from_env().run(scenario);
    assert!(report.is_pass(), "{}", report.summary());
}

#[test]
fn python_camera_frame_points_scenario_passes() {
    let scenario = catalog()
        .into_iter()
        .find(|scenario| scenario.name == "lifecycle.python_camera_frame_points.v1")
        .expect("live camera frame geometry is registered");
    let report = Runner::from_env().run(scenario);
    assert!(report.is_pass(), "{}", report.summary());
}

#[test]
fn python_rectangle_recovery_scenario_passes() {
    let scenario = catalog()
        .into_iter()
        .find(|scenario| scenario.name == "render_matrix.python_rectangle_recovery.standard.v1")
        .expect("standard portal rectangle recovery is registered");
    let report = Runner::from_env().run(scenario);
    assert!(report.is_pass(), "{}", report.summary());
}

#[test]
fn python_disc_recovery_scenario_passes() {
    let scenario = catalog()
        .into_iter()
        .find(|scenario| scenario.name == "render_matrix.python_disc_recovery.standard.v1")
        .expect("standard portal disc recovery is registered");
    let report = Runner::from_env().run(scenario);
    assert!(report.is_pass(), "{}", report.summary());
}

#[test]
fn python_rounded_rectangle_recovery_scenario_passes() {
    let scenario = catalog()
        .into_iter()
        .find(|scenario| {
            scenario.name == "render_matrix.python_rounded_rectangle_recovery.standard.v1"
        })
        .expect("standard portal quadratic rounded rectangle recovery is registered");
    let report = Runner::from_env().run(scenario);
    assert!(report.is_pass(), "{}", report.summary());
}

#[test]
fn python_scene_console_scenario_passes() {
    let scenario = catalog()
        .into_iter()
        .find(|scenario| scenario.name == "lifecycle.python_scene_console.v1")
        .expect("Python scene console scenario is registered");
    let report = Runner::from_env().run(scenario);
    assert!(report.is_pass(), "{}", report.summary());
}

#[cfg(unix)]
#[test]
fn python_worker_receipt_shutdown_scenario_passes() {
    let scenario = catalog()
        .into_iter()
        .find(|scenario| scenario.name == "failure.python_worker_receipt_shutdown.v1")
        .expect("the real worker receipt shutdown scenario is registered");
    let report = Runner::from_env().run(scenario);
    assert!(report.is_pass(), "{}", report.summary());
}

/// The typeset cache and preflight lifecycle scenarios
/// (fm-typeset-cache-preflight-wiring-1sn0), focused.
#[test]
fn typeset_cache_and_preflight_scenarios_pass() {
    for name in [
        "lifecycle.typeset_cache_warm_second_run.v1",
        "lifecycle.typeset_preflight_before_first_play.v1",
        "lifecycle.typeset_cache_retention.v1",
    ] {
        let scenario = catalog()
            .into_iter()
            .find(|scenario| scenario.name == name)
            .expect("the typeset scenario is registered");
        let report = Runner::from_env().run(scenario);
        assert!(report.is_pass(), "{}", report.summary());
    }
}

#[test]
fn phase2_and_phase3_e2e_scenarios_pass() {
    for name in [
        "lifecycle.pg6_arena_reuse_zero_allocations.v1",
        "lifecycle.python_parallel_batch_checkpoint.v1",
        "lifecycle.frozen_render_packet_pipeline.v1",
        "lifecycle.video_color_and_gallery_provenance.v1",
    ] {
        let scenario = catalog()
            .into_iter()
            .find(|scenario| scenario.name == name)
            .expect("scenario is registered");
        let report = Runner::from_env().run(scenario);
        assert!(report.is_pass(), "{}", report.summary());
    }
}

#[cfg(target_os = "linux")]
#[test]
fn linux_resource_limits_scenario_passes() {
    let scenario = catalog()
        .into_iter()
        .find(|scenario| scenario.name == "lifecycle.linux_resource_limits.v1")
        .expect("Linux resource-limit lifecycle is registered");
    let report = Runner::from_env().run(scenario);
    assert!(report.is_pass(), "{}", report.summary());
}

#[test]
fn circular_stroke_render_scenario_passes() {
    let scenario = catalog()
        .into_iter()
        .find(|scenario| scenario.name == "render_matrix.circular_strokes.standard.v1")
        .expect("native circular stroke scenario is registered");
    let report = Runner::from_env().run(scenario);
    assert!(report.is_pass(), "{}", report.summary());
}

#[test]
fn tile_binned_stroke_render_scenario_passes() {
    let scenario = catalog()
        .into_iter()
        .find(|scenario| scenario.name == "lifecycle.tile_binned_stroke_render.v1")
        .expect("tile-binned stroke scenario is registered");
    let report = Runner::from_env().run(scenario);
    assert!(report.is_pass(), "{}", report.summary());
}

#[test]
fn zero_radius_dot_scenario_passes() {
    let scenario = catalog()
        .into_iter()
        .find(|scenario| scenario.name == "lifecycle.zero_radius_dot.v1")
        .expect("the zero-radius dot scenario is registered");
    let report = Runner::from_env().run(scenario);
    assert!(report.is_pass(), "{}", report.summary());
}

#[test]
fn dot_cloud_affine_scenario_passes() {
    let scenario = catalog()
        .into_iter()
        .find(|scenario| scenario.name == "lifecycle.dot_cloud_affine_placement.v1")
        .expect("the affine dot scenario is registered");
    let report = Runner::from_env().run(scenario);
    assert!(report.is_pass(), "{}", report.summary());
}

/// The CLI builtin scenarios (fm-cli-camera-route-i1zc,
/// fm-facade-render-api-9ewt), focused.
#[test]
fn cli_builtin_scenarios_pass() {
    for name in [
        "render_matrix.retained_camera_frames.v1",
        "render_matrix.cli_camera_builtins.v1",
        "render_matrix.cli_primitive_builtins.v1",
        "determinism.cli_builtins_threads_1_4.v1",
        "render_matrix.png_sequence_frame_concurrency.v1",
    ] {
        let scenario = catalog()
            .into_iter()
            .find(|scenario| scenario.name == name)
            .expect("the CLI builtin scenario is registered");
        let report = Runner::from_env().run(scenario);
        assert!(report.is_pass(), "{}", report.summary());
    }
}

/// The re-bless protocol's tooling for the e2e lock's certified
/// render-matrix rows (GOVERNANCE.md §4). Every row renders
/// [`run_matrix_scene`], so the low preset's three certified frames stand
/// for the set; the higher presets draw the same scene at more pixels.
///
/// 1. At the old source, `FMN_E2E_FRAMES_DIR=<before>` writes each frame as
///    `<before>/render_matrix.low.<index>.rgba`.
/// 2. At the new source, bless the lock (`UPDATE_GOLDENS=1`), then run this
///    test with `FMN_E2E_REBLESS_BEFORE_DIR=<before>`: the frames are
///    stacked top to bottom, before | after, through [`side_by_side_png`],
///    and written as the lock's artefact under `REBLESS_ARTEFACT_DIR`.
///
/// Neither switch set: nothing to do (an ordinary gate run).
#[test]
fn rebless_e2e_render_matrix_frames_and_panel() {
    use fmn_conformance::golden::{REBLESS_ARTEFACT_DIR, rebless_artefact_name, side_by_side_png};
    let frames_dir = std::env::var_os("FMN_E2E_FRAMES_DIR").map(PathBuf::from);
    let before_dir = std::env::var_os("FMN_E2E_REBLESS_BEFORE_DIR").map(PathBuf::from);
    if frames_dir.is_none() && before_dir.is_none() {
        return;
    }
    let preset = PRESETS
        .iter()
        .find(|preset| preset.flag == "low")
        .expect("the low preset");
    let seed = e2e::scenario_seed("render_matrix.low.png_seq.certified.v1");
    let frames: Vec<Vec<u8>> = run_matrix_scene(seed, false)
        .expect("the matrix scene runs")
        .iter()
        .map(|packet| {
            let frame = render_packet(
                packet,
                EngineIdentity::certified(),
                preset_frame_config(preset),
                1,
            )
            .expect("the certified engine renders");
            convert_for_sink(&frame, SinkKind::PngSequence, preset.width, preset.height)
                .expect("certified frames convert")
                .as_bytes()
                .to_vec()
        })
        .collect();
    if let Some(dir) = frames_dir {
        std::fs::create_dir_all(&dir).expect("frames directory");
        for (index, frame) in frames.iter().enumerate() {
            std::fs::write(dir.join(format!("render_matrix.low.{index}.rgba")), frame)
                .expect("review frame written");
        }
    }
    let Some(before_dir) = before_dir else {
        return;
    };
    let (mut before, mut after) = (Vec::new(), Vec::new());
    for (index, frame) in frames.iter().enumerate() {
        let name = format!("render_matrix.low.{index}.rgba");
        before.extend(std::fs::read(before_dir.join(&name)).expect("before frame"));
        after.extend_from_slice(frame);
    }
    let rows = u32::try_from(frames.len()).expect("a few frames");
    let panel = side_by_side_png(preset.width, preset.height * rows, &before, &after)
        .expect("same-size before/after stacks");
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let lock = "e2e.certified.lock";
    let digest =
        fmn_hash::sha256(&std::fs::read(manifest.join("goldens").join(lock)).expect("lock"))
            .to_hex();
    let out = manifest.join("../..").join(REBLESS_ARTEFACT_DIR);
    std::fs::create_dir_all(&out).expect("re-bless artefact directory");
    std::fs::write(out.join(rebless_artefact_name(lock, &digest)), panel).expect("panel written");
}

/// Re-bless evidence for the e2e lock's `render_matrix.cli_primitive.*` and
/// `render_matrix.cli_camera.*` entries (GOVERNANCE.md §4). Opt-in with
/// `FMN_REBLESS_E2E_CLI_BUILTINS=1`, after the bless: every builtin frame is
/// decoded and stacked, and the stack goes on both sides of the lock's
/// artefact, because these entries are new. A later change to them needs a
/// before dump instead.
#[test]
fn rebless_e2e_cli_builtins_panel() {
    use fmn_conformance::golden::{REBLESS_ARTEFACT_DIR, rebless_artefact_name, side_by_side_png};
    if std::env::var("FMN_REBLESS_E2E_CLI_BUILTINS").as_deref() != Ok("1") {
        return;
    }
    let (mut stack, mut width, mut height, mut rows) = (Vec::new(), 0, 0, 0);
    for name in cli_all_builtins() {
        let (_, frames) = cli_builtin_frames(name, "1").expect("the builtin renders");
        for frame in frames {
            let image = fmn_codec::decode_png(&frame, &fmn_codec::PngLimits::default())
                .expect("a locked frame decodes");
            (width, height) = (image.width, image.height);
            stack.extend(image.rgba);
            rows += 1;
        }
    }
    let panel = side_by_side_png(width, height * rows, &stack, &stack).expect("frames stack");
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let lock = "e2e.certified.lock";
    let digest =
        fmn_hash::sha256(&std::fs::read(manifest.join("goldens").join(lock)).expect("lock"))
            .to_hex();
    let out = manifest.join("../..").join(REBLESS_ARTEFACT_DIR);
    std::fs::create_dir_all(&out).expect("re-bless artefact directory");
    std::fs::write(out.join(rebless_artefact_name(lock, &digest)), panel).expect("panel written");
}

/// Re-bless evidence for the e2e lock's `render_matrix.tex_color.v1` entry
/// (GOVERNANCE.md §4). Opt-in with `FMN_REBLESS_E2E_TEX_COLOR=1`, after the
/// bless. A new entry puts its frame on both sides of the lock's artefact. A
/// changed one names the panel that carried its old frame in
/// `FMN_REBLESS_E2E_TEX_COLOR_BEFORE`, whose right half is that frame (the
/// panels are before | 4 px divider | after).
#[test]
fn rebless_e2e_tex_color_panel() {
    if std::env::var("FMN_REBLESS_E2E_TEX_COLOR").as_deref() != Ok("1") {
        return;
    }
    let ((png, _), _) = tex_color_renders().expect("the coloured formula renders");
    write_single_frame_rebless_panel(&png, std::env::var_os("FMN_REBLESS_E2E_TEX_COLOR_BEFORE"));
}

/// Re-bless evidence for the e2e lock's `render_matrix.tex_constructs_t2.v1`
/// entry (GOVERNANCE.md §4), as for `tex_color`: opt-in with
/// `FMN_REBLESS_E2E_TEX_CONSTRUCTS_T2=1` after the bless, with
/// `FMN_REBLESS_E2E_TEX_CONSTRUCTS_T2_BEFORE` naming the old panel when the
/// entry changes.
#[test]
fn rebless_e2e_tex_constructs_t2_panel() {
    if std::env::var("FMN_REBLESS_E2E_TEX_CONSTRUCTS_T2").as_deref() != Ok("1") {
        return;
    }
    let (png, _, _) = tex_constructs_t2_render().expect("the constructs sheet renders");
    write_single_frame_rebless_panel(
        &png,
        std::env::var_os("FMN_REBLESS_E2E_TEX_CONSTRUCTS_T2_BEFORE"),
    );
}

/// Re-bless evidence for the e2e lock's `render_matrix.tex_cancel_frak.v1`
/// entry, as for `tex_constructs_t2`: opt-in with
/// `FMN_REBLESS_E2E_TEX_CANCEL_FRAK=1` after the bless, with
/// `FMN_REBLESS_E2E_TEX_CANCEL_FRAK_BEFORE` naming the old panel when the
/// entry changes.
#[test]
fn rebless_e2e_tex_cancel_frak_panel() {
    if std::env::var("FMN_REBLESS_E2E_TEX_CANCEL_FRAK").as_deref() != Ok("1") {
        return;
    }
    let (png, _, _) = tex_sheet_render(TEX_CANCEL_FRAK_MATH, TEX_CANCEL_FRAK_TEXT)
        .expect("the Fraktur and cancel sheet renders");
    write_single_frame_rebless_panel(
        &png,
        std::env::var_os("FMN_REBLESS_E2E_TEX_CANCEL_FRAK_BEFORE"),
    );
}

/// Write a one-frame lock entry's re-bless panel, before | 4 px divider |
/// after, as the e2e lock's artefact. A new entry (`before` unset) puts its
/// frame on both sides; a changed one takes the accepted right half of the
/// previous panel. Its left half belongs to the earlier adjudication.
fn write_single_frame_rebless_panel(png: &[u8], before: Option<std::ffi::OsString>) {
    use fmn_conformance::golden::{REBLESS_ARTEFACT_DIR, rebless_artefact_name, side_by_side_png};
    let image = fmn_codec::decode_png(png, &fmn_codec::PngLimits::default())
        .expect("the locked frame decodes");
    let before = match before {
        None => image.rgba.clone(),
        Some(path) => {
            let old = fmn_codec::decode_png(
                &std::fs::read(&path).expect("the before panel"),
                &fmn_codec::PngLimits::default(),
            )
            .expect("the before panel decodes");
            accepted_frame_from_rebless_panel(&old, image.width, image.height)
        }
    };
    let panel = side_by_side_png(image.width, image.height, &before, &image.rgba)
        .expect("before and after are the same size");
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let lock = "e2e.certified.lock";
    let digest =
        fmn_hash::sha256(&std::fs::read(manifest.join("goldens").join(lock)).expect("lock"))
            .to_hex();
    let out = manifest.join("../..").join(REBLESS_ARTEFACT_DIR);
    std::fs::create_dir_all(&out).expect("re-bless artefact directory");
    std::fs::write(out.join(rebless_artefact_name(lock, &digest)), panel).expect("panel written");
}

fn accepted_frame_from_rebless_panel(
    panel: &fmn_codec::DecodedPng,
    width: u32,
    height: u32,
) -> Vec<u8> {
    assert!(
        panel.width == 2 * width + 4 && panel.height == height,
        "the before panel is {}x{}, not a {}x{} pair",
        panel.width,
        panel.height,
        width,
        height
    );
    let row = panel.width as usize * 4;
    let accepted_start = (width as usize + 4) * 4;
    panel
        .rgba
        .chunks_exact(row)
        .flat_map(|line| line[accepted_start..].iter().copied())
        .collect()
}

#[test]
fn rebless_before_is_the_previous_accepted_frame() {
    use fmn_conformance::golden::side_by_side_png;
    let earlier = [1, 2, 3, 255, 4, 5, 6, 255, 7, 8, 9, 255, 10, 11, 12, 255];
    let accepted = [
        13, 14, 15, 255, 16, 17, 18, 255, 19, 20, 21, 255, 22, 23, 24, 255,
    ];
    let bytes = side_by_side_png(2, 2, &earlier, &accepted).expect("distinct prior frames");
    let panel = fmn_codec::decode_png(&bytes, &fmn_codec::PngLimits::default())
        .expect("the native review panel decodes");
    assert_eq!(accepted_frame_from_rebless_panel(&panel, 2, 2), accepted);
}

#[test]
fn cli_pure_dispatch_scenario_passes() {
    let scenario = catalog()
        .into_iter()
        .find(|scenario| scenario.name == "determinism.cli_pure_segments.v1")
        .expect("native CLI pure dispatch scenario is registered");
    let report = Runner::from_env().run(scenario);
    assert!(report.is_pass(), "{}", report.summary());
}

#[test]
fn documented_config_effects_scenario_passes() {
    let scenario = catalog()
        .into_iter()
        .find(|scenario| scenario.name == "lifecycle.documented_config_effects.v1")
        .expect("documented configuration effects scenario is registered");
    let report = Runner::from_env().run(scenario);
    assert!(report.is_pass(), "{}", report.summary());
}

#[test]
fn control_panel_frame_lifecycle_scenario_passes() {
    let scenario = catalog()
        .into_iter()
        .find(|scenario| scenario.name == "lifecycle.control_panel_frame.v1")
        .expect("current-frame control panel lifecycle is registered");
    let report = Runner::from_env().run(scenario);
    assert!(report.is_pass(), "{}", report.summary());
}
