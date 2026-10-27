//! Process-level steady-state heap allocations per frame, PG-6's tripwire
//! (fm-counting-allocator-harness-r4-ndz8).
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
//! so every measurement runs in this one test, sequentially.
//!
//! Each route runs on one thread and on a team of four. Every render is
//! repeated and the repetition with the fewest events is the measurement;
//! the NDJSON record publishes the spread between the fewest and the most.
//! On one thread the pure routes repeat exactly. A scene updater does not:
//! identical renders with one differ by hundreds of events, which the
//! informational updater route shows.
//!
//! The rates are informational until the per-frame allocators the bead names
//! are gone (fm-render-team-pool-j9ub, fm-soa-mirrors-dead-p0gc); the test
//! prints them as NDJSON. The planted defect is not informational: on one
//! thread, fmn's test-only per-frame allocation (the
//! `planted-frame-allocation` feature, zero unless this test sets it) must
//! raise the pure route's measured rate by exactly one. A planted allocation
//! in scene code would move the scene onto the stateful route, whose spread
//! is larger than the signal.
#![forbid(unsafe_code)]
#![allow(clippy::expect_used, clippy::panic, clippy::print_stdout)]

use fmn::prelude::*;
use stats_alloc::{INSTRUMENTED_SYSTEM, Region, StatsAlloc};
use std::alloc::System;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};

#[global_allocator]
static GLOBAL: &StatsAlloc<System> = &INSTRUMENTED_SYSTEM;

/// The render teams every route is measured with.
const THREADS: [u32; 2] = [1, 4];

/// Repetitions of every measured render.
const REPEATS: usize = 5;

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
/// one scene, printed as one NDJSON record.
#[allow(clippy::cast_precision_loss)]
fn rate(route: &str, threads: u32, short: Sample, long: Sample) -> f64 {
    assert!(
        long.frames > short.frames,
        "{route}: the long render has {} frames, the short one {}",
        long.frames,
        short.frames
    );
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
    events
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

/// The shipped CLI, in process: `@builtin circle_shift.v1` at `fps`.
fn cli(format: &str, fps: &str, threads: u32) -> u64 {
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
        "circle_shift.v1",
    ]);
    assert_eq!(
        run.code, 0,
        "CLI render failed: {}{}",
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

#[test]
fn steady_state_allocations_per_frame() {
    // Warm both routes once so lazily built process state (font faces, the
    // render team's first plan) is outside every measured pair.
    for threads in THREADS {
        facade(RenderFormat::Y4m, 0.2, Variant::Pure, threads);
        cli("y4m", "15", threads);
    }

    for threads in THREADS {
        for (format, name) in [
            (RenderFormat::Y4m, "y4m"),
            (RenderFormat::PngSequence, "png"),
        ] {
            rate(
                &format!("facade-pure-{name}"),
                threads,
                sample(|| facade(format, 1.0, Variant::Pure, threads)),
                sample(|| facade(format, 2.0, Variant::Pure, threads)),
            );
        }
        for format in ["y4m", "png_sequence"] {
            rate(
                &format!("cli-circle-shift-{format}"),
                threads,
                sample(|| cli(format, "15", threads)),
                sample(|| cli(format, "30", threads)),
            );
        }
    }

    // A scene updater, informational: its record's spread shows how far
    // identical renders differ (fm-updater-allocation-spread-r4-viw6).
    rate(
        "facade-updater-y4m",
        1,
        sample(|| facade(RenderFormat::Y4m, 1.0, Variant::Updater, 1)),
        sample(|| facade(RenderFormat::Y4m, 2.0, Variant::Updater, 1)),
    );

    // The planted defect: on one thread, fmn's test-only per-frame allocation
    // in the frame loop must raise the pure route's rate by exactly one.
    let quiet = rate(
        "facade-pure-y4m",
        1,
        sample(|| facade(RenderFormat::Y4m, 1.0, Variant::Pure, 1)),
        sample(|| facade(RenderFormat::Y4m, 2.0, Variant::Pure, 1)),
    );
    fmn::rendering::PLANTED_FRAME_ALLOCATIONS.store(1, Ordering::Relaxed);
    let planted = rate(
        "facade-planted-y4m",
        1,
        sample(|| facade(RenderFormat::Y4m, 1.0, Variant::Pure, 1)),
        sample(|| facade(RenderFormat::Y4m, 2.0, Variant::Pure, 1)),
    );
    fmn::rendering::PLANTED_FRAME_ALLOCATIONS.store(0, Ordering::Relaxed);
    // The pure route's fewest events can still differ by one between two
    // positions in the process, so the shift may miss 1 by one event over
    // the 30 measured frames (2 s against 1 s at 30 fps), never more: 30
    // planted events against at most 1.
    let moved = planted - quiet;
    let one_event = 1.0 / 30.0;
    assert!(
        (moved - 1.0).abs() <= one_event + 1e-9,
        "one planted allocation per frame moved the one-thread rate by {moved:.3}, not 1"
    );
}
