//! Process-level steady-state heap allocations per frame, PG-6's tripwire
//! (fm-counting-allocator-harness-r4-ndz8), registered as the e2e scenario
//! `lifecycle.steady_state_allocations.v1`.
//!
//! Every allocation in this test binary passes through `stats_alloc`'s
//! counting wrapper over the system allocator: a dev-only dependency admitted
//! under ADR-0003, which never reaches a shipped artifact. The engine's own
//! arena ledger (`perf_pg6`) sees only the raster arena; this sees every
//! allocation the process makes while it renders, on every thread.
//!
//! A render's fixed costs (construction, warm-up, publication) are the same
//! for a short and a long play of one scene. So the difference of the two
//! counts, over the difference of their frames, is the steady-state cost of
//! one frame, with no hook in production code. The counter is process-wide,
//! so every measurement runs in this binary's one test, sequentially. That
//! is why the scenario is registered here, over the e2e harness API, and not
//! in `e2e_scenarios.rs`, whose tests run in parallel.
//!
//! The routes:
//! - `fmn::render` of a sliding square, y4m and PNG, on one thread and on a
//!   team of four;
//! - the shipped CLI on every scene of the 25-scene primitive corpus (y4m, one
//!   thread), and on `circle_shift.v1` with PNG output and four threads;
//! - a Python scene through the embedded portal (y4m, one and four threads).
//!   CPython's own allocator is not Rust's global allocator, so this route
//!   counts the engine's and the bridge's allocations, not the interpreter's.
//!
//! Every render is repeated and the repetition with the fewest events is the
//! measurement; each record publishes the spread between the fewest and the
//! most. On one thread the pure routes repeat exactly. A scene updater does
//! not: identical renders with one differ by hundreds of events, which the
//! informational updater route shows.
//!
//! The rates are informational until the per-frame allocators the bead names
//! are gone (fm-render-team-pool-j9ub, fm-soa-mirrors-dead-p0gc). Each one is a
//! scenario log event and an NDJSON line on stdout. The planted defect is not
//! informational: on one thread, fmn's test-only per-frame allocation (the
//! `planted-frame-allocation` feature, zero unless this test sets it) must
//! raise the pure route's measured rate by exactly one. A planted allocation
//! in scene code would move the scene onto the stateful route, whose spread
//! is larger than the signal.
#![forbid(unsafe_code)]
#![allow(clippy::expect_used, clippy::panic, clippy::print_stdout)]

use fmn::prelude::*;
use fmn_conformance::e2e::{
    Assertion, FieldPred, Invocation, LogEvent, LogExpect, RunCtx, RunOutcome, Runner,
    ScenarioClass, ScenarioError, ScenarioSpec, StructuralAssert, Surface,
};
use stats_alloc::{INSTRUMENTED_SYSTEM, Region, StatsAlloc};
use std::alloc::System;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};

#[global_allocator]
static GLOBAL: &StatsAlloc<System> = &INSTRUMENTED_SYSTEM;

/// The render teams the facade and portal routes are measured with.
const THREADS: [u32; 2] = [1, 4];

/// Repetitions of every measured render.
const REPEATS: usize = 5;

/// The rate records one run makes: facade 4, CLI corpus 25, CLI
/// `circle_shift.v1` 3, portal 2, updater 1, planted pair 2.
const ROUTES: u64 = 37;

/// One render: its frames and the allocation events (fresh allocations plus
/// reallocations) and bytes the process made while it ran, the fewest of
/// [`REPEATS`] repetitions, and the most events any repetition made.
#[derive(Clone, Copy, Debug)]
struct Sample {
    frames: u64,
    events: usize,
    bytes: usize,
    most_events: usize,
}

fn sample(run: impl Fn() -> u64) -> Sample {
    let runs: Vec<Sample> = (0..REPEATS)
        .map(|_| {
            let region = Region::new(GLOBAL);
            let frames = run();
            let change = region.change();
            let events = change.allocations + change.reallocations;
            Sample {
                frames,
                events,
                bytes: change.bytes_allocated,
                most_events: events,
            }
        })
        .collect();
    let most_events = runs.iter().map(|run| run.events).max().expect("repeats");
    let fewest = runs
        .into_iter()
        .min_by_key(|run| run.events)
        .expect("repeats");
    Sample {
        most_events,
        ..fewest
    }
}

/// Steady-state events and bytes per frame from a short and a long render of
/// one scene, recorded as a scenario event and printed as one NDJSON line.
#[allow(clippy::cast_precision_loss)]
fn rate(
    ctx: &mut RunCtx,
    route: &str,
    threads: u32,
    short: Sample,
    long: Sample,
) -> Result<f64, ScenarioError> {
    if long.frames <= short.frames {
        return Err(ScenarioError::new(format!(
            "{route}: the long render has {} frames, the short one {}",
            long.frames, short.frames
        )));
    }
    let frames = (long.frames - short.frames) as f64;
    let events = (long.events as f64 - short.events as f64) / frames;
    let bytes = (long.bytes as f64 - short.bytes as f64) / frames;
    println!(
        "{{\"schema\":\"fmn.steady-allocations.v1\",\"route\":\"{route}\",\"threads\":{threads},\
         \"frames\":[{},{}],\"events\":[{},{}],\"spread\":[{},{}],\"events_per_frame\":{events:.3},\
         \"bytes_per_frame\":{bytes:.1}}}",
        short.frames,
        long.frames,
        short.events,
        long.events,
        short.most_events - short.events,
        long.most_events - long.events
    );
    ctx.event(
        LogEvent::new("e2e.steady_allocations.rate")
            .field("route", route)
            .field("threads", u64::from(threads))
            .field("frames_short", short.frames)
            .field("frames_long", long.frames)
            .field("events_short", short.events)
            .field("events_long", long.events)
            .field("spread_short", short.most_events - short.events)
            .field("spread_long", long.most_events - long.events)
            .field("events_per_frame", format!("{events:.3}"))
            .field("bytes_per_frame", format!("{bytes:.1}")),
    );
    Ok(events)
}

/// A fresh, never-existing output path (publication is no-clobber).
fn output(name: &str) -> PathBuf {
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!(
        "steady-allocations-{}-{}-{name}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ))
}

/// What a [`Slide`] carries besides its moving square.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Variant {
    /// Nothing more: the pure route.
    Pure,
    /// A still square with a scene updater that does nothing.
    Updater,
}

/// A square sliding right for `seconds`, as a [`Variant`] dresses it.
struct Slide {
    seconds: f64,
    variant: Variant,
}

impl SceneConstruct for Slide {
    fn construct(&mut self, stage: &mut Stage<'_>) -> fmn::Result<()> {
        let square = stage.add(Square::new().side_length(1.0).color(BLUE))?;
        if self.variant == Variant::Updater {
            let still = stage.add(Square::new().side_length(0.5).color(RED))?;
            stage
                .arena_mut()
                .add_updater(still, |_, _| {}, false)
                .map_err(fmn::Error::Stage)?;
        }
        let slide = square
            .animate()
            .set_anim_args(AnimateArgs {
                run_time: Some(self.seconds),
                rate_func: Some(fmn::core::rate::linear),
                ..AnimateArgs::default()
            })?
            .shift([2.0, 0.0, 0.0])?;
        stage.play(slide)?;
        Ok(())
    }
}

/// `fmn::render` of [`Slide`] at 128x72, 30 fps, a fixed thread team.
fn facade(format: RenderFormat, seconds: f64, variant: Variant, threads: u32) -> u64 {
    let mut options =
        RenderOptions::with_format(output("facade"), format).expect("bundled configuration");
    options.config.camera.resolution = (128, 72);
    options.config.camera.fps = 30;
    options.config.render.threads = ThreadPolicy::Fixed(threads);
    options.typeset_cache = false;
    render(&mut Slide { seconds, variant }, options)
        .expect("facade render")
        .artifact
        .frame_count
}

/// The shipped CLI, in process: the builtin `scene` at `fps`.
fn cli(scene: &str, format: &str, fps: &str, threads: u32) -> u64 {
    let dir = output("cli");
    let dir = dir.to_str().expect("UTF-8 scratch path");
    let threads = threads.to_string();
    let run = fmn_cli::run([
        "--robot",
        "--format",
        format,
        "--resolution",
        "128x72",
        "--fps",
        fps,
        "--threads",
        &threads,
        "--video_dir",
        dir,
        fmn_cli::BUILTIN_SCENE_SOURCE,
        scene,
    ]);
    assert_eq!(
        run.code, 0,
        "CLI render of {scene} failed: {}{}",
        run.stdout, run.stderr
    );
    let record = run
        .stdout
        .lines()
        .find(|line| line.contains("\"kind\":\"render\""))
        .expect("a render record");
    record
        .split("\"frames\":")
        .nth(1)
        .and_then(|rest| rest.split([',', '}']).next())
        .and_then(|value| value.parse().ok())
        .expect("the render record's frame count")
}

/// A Python scene through the embedded portal: a square sliding right for
/// `seconds`, to y4m at 128x72 and 30 fps.
fn portal(seconds: f64, threads: u32) -> u64 {
    let threads = usize::try_from(threads).expect("a small team");
    manimlib::render_portal_slide(&output("portal.y4m"), seconds, threads)
        .unwrap_or_else(|error| panic!("portal render: {error}"))
}

/// The scenario body: every route's rate, then the planted defect.
fn measure(ctx: &mut RunCtx) -> Result<RunOutcome, ScenarioError> {
    // Warm every route once so lazily built process state (font faces, the
    // render team's first plan, the portal's first import) is outside every
    // measured pair.
    for threads in THREADS {
        facade(RenderFormat::Y4m, 0.2, Variant::Pure, threads);
        cli("circle_shift.v1", "y4m", "15", threads);
        portal(0.2, threads);
    }
    let mut routes = 0;

    for threads in THREADS {
        for (format, name) in [
            (RenderFormat::Y4m, "y4m"),
            (RenderFormat::PngSequence, "png"),
        ] {
            rate(
                ctx,
                &format!("facade-pure-{name}"),
                threads,
                sample(|| facade(format, 1.0, Variant::Pure, threads)),
                sample(|| facade(format, 2.0, Variant::Pure, threads)),
            )?;
            routes += 1;
        }
    }

    for scene in fmn::builtins::PRIMITIVE_SCENE_NAMES {
        rate(
            ctx,
            &format!("cli-corpus-{scene}-y4m"),
            1,
            sample(|| cli(scene, "y4m", "15", 1)),
            sample(|| cli(scene, "y4m", "30", 1)),
        )?;
        routes += 1;
    }
    for (format, threads) in [("y4m", 4), ("png_sequence", 1), ("png_sequence", 4)] {
        rate(
            ctx,
            &format!("cli-circle-shift-{format}"),
            threads,
            sample(|| cli("circle_shift.v1", format, "15", threads)),
            sample(|| cli("circle_shift.v1", format, "30", threads)),
        )?;
        routes += 1;
    }

    for threads in THREADS {
        rate(
            ctx,
            "portal-slide-y4m",
            threads,
            sample(|| portal(1.0, threads)),
            sample(|| portal(2.0, threads)),
        )?;
        routes += 1;
    }

    // A scene updater, informational: its record's spread shows how far
    // identical renders differ (fm-updater-allocation-spread-r4-viw6).
    rate(
        ctx,
        "facade-updater-y4m",
        1,
        sample(|| facade(RenderFormat::Y4m, 1.0, Variant::Updater, 1)),
        sample(|| facade(RenderFormat::Y4m, 2.0, Variant::Updater, 1)),
    )?;
    routes += 1;

    // The planted defect: on one thread, fmn's test-only per-frame allocation
    // in the frame loop must raise the pure route's rate by exactly one.
    let quiet = rate(
        ctx,
        "facade-pure-y4m",
        1,
        sample(|| facade(RenderFormat::Y4m, 1.0, Variant::Pure, 1)),
        sample(|| facade(RenderFormat::Y4m, 2.0, Variant::Pure, 1)),
    )?;
    fmn::rendering::PLANTED_FRAME_ALLOCATIONS.store(1, Ordering::Relaxed);
    let planted = rate(
        ctx,
        "facade-planted-y4m",
        1,
        sample(|| facade(RenderFormat::Y4m, 1.0, Variant::Pure, 1)),
        sample(|| facade(RenderFormat::Y4m, 2.0, Variant::Pure, 1)),
    );
    fmn::rendering::PLANTED_FRAME_ALLOCATIONS.store(0, Ordering::Relaxed);
    let planted = planted?;
    routes += 2;
    // The pure route's fewest events can still differ by one between two
    // positions in the process, so the shift may miss 1 by one event over
    // the 30 measured frames (2 s against 1 s at 30 fps), never more: 30
    // planted events against at most 1.
    let moved = planted - quiet;
    let exact = (moved - 1.0).abs() <= 1.0 / 30.0 + 1e-9;
    ctx.event(
        LogEvent::new("e2e.steady_allocations.planted")
            .field("moved", format!("{moved:.3}"))
            .field("exact", exact),
    );
    Ok(RunOutcome::ok()
        .with_counter("routes_measured", routes)
        .with_counter("planted_shift_exact", u64::from(exact)))
}

#[test]
fn steady_state_allocations_per_frame() {
    let scenario = ScenarioSpec::new(
        "lifecycle.steady_state_allocations.v1",
        ScenarioClass::LifecycleDrill,
        Surface::RustApi,
        Invocation::new(measure),
    )
    .assertions(vec![
        Assertion::ExitCode(0),
        Assertion::Structural(StructuralAssert::CounterEq("routes_measured", ROUTES)),
        Assertion::Structural(StructuralAssert::CounterEq("planted_shift_exact", 1)),
        Assertion::NdjsonSchema,
    ])
    .logs(vec![
        LogExpect::span_present(
            "e2e.steady_allocations.rate",
            vec![FieldPred::str_eq("route", "portal-slide-y4m")],
        ),
        LogExpect::span_present(
            "e2e.steady_allocations.rate",
            vec![FieldPred::str_eq(
                "route",
                "cli-corpus-layered_polygon.v1-y4m",
            )],
        ),
        LogExpect::span_present(
            "e2e.steady_allocations.planted",
            vec![FieldPred::bool_eq("exact", true)],
        ),
    ]);
    let report = Runner::from_env().run(scenario);
    assert!(report.is_pass(), "{}", report.summary());
}
