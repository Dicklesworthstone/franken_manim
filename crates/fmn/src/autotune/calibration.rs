//! Explicit standard-mode raster-tile calibration.
//!
//! This probe measures one retained 2D renderer team, not the frame pipeline,
//! team-count policy, encoder, or end-to-end scene throughput. The requested
//! resolution is kept: downscaling changes the number of fine-tile row bands
//! and would confound tile selection with available worker parallelism.
//!
//! Each of three interleaved rounds constructs only one candidate renderer at
//! a time. An untimed frame starts its workers and warms its retained state;
//! the timed frame moves real layered curves. The renderer is then dropped.
//! At most the baseline first team's workers are alive, never three parked
//! teams. Every pose must satisfy the renderer's versioned maximum/RMS linear
//! channel error limits across candidates, with finite normalized RGBA16F data.
//!
//! The handed-in monotonic clock bounds the probe cooperatively. Checks occur
//! between setup steps and before/after every frame. An in-flight frame is not
//! preempted: one bounded frame may finish beyond the budget. An incomplete
//! probe never publishes a profile.

use std::fmt;
use std::sync::Arc;
use std::time::Duration;

use fmn_cache::{CacheError, CacheKey};
use fmn_config::Config;
use fmn_core::color::{LinearRgba, Srgb};
use fmn_frame::{FrameBuffer, FrameLayout, PixelFormat};
use fmn_library::{CubicBezier, Ellipse, VStyle};
use fmn_mobject::{Mob, Stage};
use fmn_platform::clock::Clock;
use fmn_platform::fs::FileSystem;
use fmn_platform::topology::HardwareTopology;
use fmn_render::{
    EngineIdentity, FrameConfig, RetainedFrameRenderer, RetainedFrameRendererConfig, ScreenMap,
    Tier, Tiling, Viewport,
};
use fmn_runtime::{
    AutotuneProfile, Determinism, ExecutionEngine, ExecutionPlan, PlanError, PlanRequest,
    SurfaceSpec, TopologyFingerprint,
};

use super::{AutotuneError, load_profile, profile_key, store_profile};

/// Versioned workload identity; changes require the parent profile-key version
/// to change before measurements from this workload can share cached entries.
pub const CALIBRATION_WORKLOAD: &str = "layered-curves-v1";
/// Largest admitted probe surface. Requested dimensions are never downscaled.
pub const MAX_CALIBRATION_PIXELS: u64 = 1920 * 1080;
/// Pixel admission rule reported by doctor receipts. This uses the renderer's
/// versioned maximum/RMS linear-channel limits, not a certified bit or SSIM
/// equivalence claim. The fixed SDR probe admits only finite `[0,1]` channels.
pub const PIXEL_EQUIVALENCE_POLICY: &str = "linear-rgba16f-channel-v1: matching tight layout; finite channels in [0,1]; max absolute error <= 0.092; RMS error <= 0.00042";
/// Largest admitted absolute linear-channel error, from the renderer's v1 budget.
pub const PIXEL_EQUIVALENCE_MAX_CHANNEL_ERROR: f64 =
    fmn_render::FAST_VISUAL_BUDGET_V1_MAX_CHANNEL_ERROR;
/// Largest admitted RMS linear-channel error, from the renderer's v1 budget.
pub const PIXEL_EQUIVALENCE_RMS_CHANNEL_ERROR: f64 =
    fmn_render::FAST_VISUAL_BUDGET_V1_RMS_CHANNEL_ERROR;

const REPETITIONS: usize = 3;
const CANDIDATES: [(u32, u32); 3] = [(8, 64), (16, 128), (32, 256)];
const MAX_WORKERS: usize = 32;
const IMPROVEMENT_PERCENT: u128 = 5;

/// Outcome at the immutable profile boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CalibrationStatus {
    /// A new complete measurement was accepted by the store. Identical bytes
    /// may also have been accepted from a concurrent producer.
    New,
    /// The existing valid entry was reused without reading the probe clock.
    Reused,
    /// Another producer's different immutable profile won publication.
    ConcurrentWinner,
}

impl CalibrationStatus {
    /// Stable spelling for doctor receipts.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::New => "new",
            Self::Reused => "reused",
            Self::ConcurrentWinner => "concurrent-winner",
        }
    }
}

/// Real retained-renderer timings for one tile geometry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CalibrationSample {
    /// Fine-tile edge in pixels.
    pub fine_tile: u32,
    /// Macrotile edge in pixels.
    pub macro_tile: u32,
    /// Three moving-frame durations, in interleaved round order.
    pub elapsed_ns: [u64; REPETITIONS],
    /// Median of the three durations. Initialization is not timed.
    pub median_ns: u64,
}

/// Evidence from this attempt and the profile that subsequent renders consume.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CalibrationReport {
    /// Whether this attempt measured, reused, or lost publication to an incumbent.
    pub status: CalibrationStatus,
    /// The exact shared CLI/native profile address.
    pub profile_key: CacheKey,
    /// Full topology identity used by the planner and cache key.
    pub topology: TopologyFingerprint,
    /// Requested, unscaled surface.
    pub surface: SurfaceSpec,
    /// The unchanged baseline first-team width used by every candidate.
    pub workers: usize,
    /// Actual compiled fast-CPU build tier.
    pub build_tier: Tier,
    /// Actual renderer version.
    pub renderer_version: u32,
    /// Accepted immutable profile; on a conflict this is the incumbent's value.
    pub profile: AutotuneProfile,
    /// This attempt's timings, empty when an existing entry was reused. A
    /// concurrent winner may have different timings from this attempt.
    pub samples: Vec<CalibrationSample>,
    /// Probe elapsed time, excluding profile lookup/publication. Zero on reuse.
    pub elapsed: Duration,
    /// Successfully completed warm and moving frames. Zero on reuse.
    pub rendered_frames: u32,
}

/// A named refusal from explicit calibration, never a silent rendering fallback.
#[derive(Debug)]
pub enum CalibrationError {
    /// The requested engine, surface, or budget cannot be calibrated here.
    Unsupported(&'static str),
    /// The shared cache/key boundary refused the explicit operation.
    Autotune(AutotuneError),
    /// Baseline planning failed before the probe or cache was consulted.
    Plan(PlanError),
    /// The cooperative budget expired. This attempt published no profile.
    BudgetExceeded {
        /// Declared probe budget.
        budget: Duration,
        /// Elapsed time observed at the boundary.
        elapsed: Duration,
        /// Frames fully returned before the refusal.
        completed_frames: u32,
    },
    /// The host violated its monotonic-clock contract.
    ClockWentBackwards,
    /// The clock did not resolve a positive, representable frame duration.
    UnmeasurableClock,
    /// Geometry construction or the actual retained renderer failed.
    Probe(String),
    /// A probe frame had invalid pixels/layout or exceeded the numeric error limits.
    PixelMismatch {
        /// One-based moving-pose index.
        round: usize,
        /// Fine tile of the first candidate in this interleaved round.
        reference_fine_tile: u32,
        /// Fine tile of the candidate that differed.
        candidate_fine_tile: u32,
    },
    /// An immutable conflict occurred, but its winner could no longer be read.
    ConcurrentWinnerUnavailable,
}

impl fmt::Display for CalibrationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unsupported(reason) => write!(f, "raster-tile calibration unavailable: {reason}"),
            Self::Autotune(error) => write!(f, "raster-tile calibration cache refusal: {error}"),
            Self::Plan(error) => write!(f, "raster-tile calibration planning failed: {error}"),
            Self::BudgetExceeded { budget, elapsed, completed_frames } => write!(
                f,
                "raster-tile calibration budget {budget:?} exhausted after {elapsed:?} \
                 and {completed_frames} completed frames; this attempt published no profile"
            ),
            Self::ClockWentBackwards => f.write_str("raster-tile calibration clock moved backwards"),
            Self::UnmeasurableClock => f.write_str(
                "raster-tile calibration clock did not resolve a positive frame duration",
            ),
            Self::Probe(reason) => write!(f, "raster-tile calibration probe failed: {reason}"),
            Self::PixelMismatch { round, reference_fine_tile, candidate_fine_tile } => write!(
                f,
                "raster-tile calibration pose {round} violated {PIXEL_EQUIVALENCE_POLICY} \
                 between fine tiles {reference_fine_tile} and {candidate_fine_tile}"
            ),
            Self::ConcurrentWinnerUnavailable => f.write_str(
                "raster-tile calibration lost immutable publication but the winning profile is unavailable",
            ),
        }
    }
}

impl std::error::Error for CalibrationError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Autotune(error) => Some(error),
            Self::Plan(error) => Some(error),
            _ => None,
        }
    }
}

impl From<AutotuneError> for CalibrationError {
    fn from(error: AutotuneError) -> Self {
        Self::Autotune(error)
    }
}

impl From<PlanError> for CalibrationError {
    fn from(error: PlanError) -> Self {
        Self::Plan(error)
    }
}

/// Calibrate only standard fast-CPU raster tile sizes using the shared cache.
///
/// Team placement, team count, frame slots, and per-worker scratch remain the
/// baseline planner's choices. A candidate needs at least a five-percent median
/// improvement over the baseline tiles; otherwise the completed measurement
/// stores the baseline. This is a narrow calibration result, not a throughput
/// or speedup claim about arbitrary scenes or the complete scheduler.
///
/// Certified/annex requests are rejected before cache or clock access. A valid
/// cached entry returns immediately. Only a complete, pixel-equivalent probe
/// may reach immutable publication; clearing the cache permits retuning.
///
/// # Errors
/// Returns CalibrationError for invalid requests, explicit cache refusals,
/// budget exhaustion, clock faults, renderer failures, or invalid/non-equivalent pixels.
/// Cache and planner errors retain their original sources.
pub fn calibrate(
    fs: Arc<dyn FileSystem>,
    clock: Arc<dyn Clock>,
    config: &Config,
    request: PlanRequest,
    topology: &HardwareTopology,
    budget: Duration,
) -> Result<CalibrationReport, CalibrationError> {
    let baseline = ExecutionPlan::derive(request, topology, None)?;
    if baseline.determinism != Determinism::Standard || baseline.engine != ExecutionEngine::FastCpu
    {
        return Err(CalibrationError::Unsupported(
            "only standard fast-CPU requests can be measured",
        ));
    }
    let surface = request.surface();
    let team = baseline
        .render_teams
        .first()
        .ok_or(CalibrationError::Unsupported(
            "the baseline contains no render team",
        ))?;
    let workers = team.threads();
    let key = profile_key(config, request, topology)?;
    let identity = EngineIdentity::fast();
    let mut report = CalibrationReport {
        status: CalibrationStatus::Reused,
        profile_key: key,
        topology: baseline.topology_fingerprint,
        surface,
        workers,
        build_tier: identity.tier,
        renderer_version: identity.renderer_version,
        profile: AutotuneProfile {
            topology: baseline.topology_fingerprint,
            threads_per_render_team: workers,
            frames_in_flight: baseline.frames_in_flight,
            fine_tile: baseline.fine_tile,
            macro_tile: baseline.macro_tile,
            scratch_bytes_per_worker: team.scratch_bytes_per_worker,
        },
        samples: Vec::new(),
        elapsed: Duration::ZERO,
        rendered_frames: 0,
    };
    if let Some(profile) = load_profile(fs.as_ref(), config, request, topology)? {
        report.profile = profile;
        return Ok(report);
    }

    if u64::from(surface.width) * u64::from(surface.height) > MAX_CALIBRATION_PIXELS {
        return Err(CalibrationError::Unsupported(
            "requested surface exceeds the 1920x1080-pixel probe ceiling; no downscaling is performed",
        ));
    }
    if budget.is_zero() {
        return Err(CalibrationError::Unsupported(
            "the probe budget must be nonzero",
        ));
    }
    if workers > MAX_WORKERS {
        return Err(CalibrationError::Unsupported(
            "the baseline first team exceeds 32 workers; its width will not be reduced",
        ));
    }
    let baseline_index = CANDIDATES
        .iter()
        .position(|&(fine, macro_tile)| {
            fine == baseline.fine_tile && macro_tile == baseline.macro_tile
        })
        .ok_or(CalibrationError::Unsupported(
            "the baseline tile geometry is outside the declared candidate set",
        ))?;

    let scale = f64::from(surface.height) / 8.0;
    let frame = FrameConfig::new(
        Viewport {
            width: surface.width,
            height: surface.height,
        },
        ScreenMap {
            scale,
            origin: [
                f64::from(surface.width) / 2.0,
                f64::from(surface.height) / 2.0,
            ],
            y_up: true,
        },
        LinearRgba {
            r: 0.018,
            g: 0.022,
            b: 0.030,
            a: 1.0,
        },
    )
    .with_aa_policy(config.render.aa);
    let measured = measure_candidates(clock.as_ref(), budget, frame, workers)?;
    let selected = select_candidate(&measured.samples, baseline_index);
    report.profile.fine_tile = measured.samples[selected].fine_tile;
    report.profile.macro_tile = measured.samples[selected].macro_tile;
    report.samples = measured.samples.into_iter().collect();
    report.elapsed = measured.elapsed;
    report.rendered_frames = measured.rendered_frames;
    publish(fs, clock, config, request, topology, report)
}

fn publish(
    fs: Arc<dyn FileSystem>,
    clock: Arc<dyn Clock>,
    config: &Config,
    request: PlanRequest,
    topology: &HardwareTopology,
    mut report: CalibrationReport,
) -> Result<CalibrationReport, CalibrationError> {
    match store_profile(fs.clone(), clock, config, request, topology, report.profile) {
        Ok(()) => report.status = CalibrationStatus::New,
        Err(AutotuneError::Cache(CacheError::KeyConflict(_))) => {
            report.profile = load_profile(fs.as_ref(), config, request, topology)?
                .ok_or(CalibrationError::ConcurrentWinnerUnavailable)?;
            report.status = CalibrationStatus::ConcurrentWinner;
        }
        Err(error) => return Err(error.into()),
    }
    Ok(report)
}

struct Measurements {
    samples: [CalibrationSample; CANDIDATES.len()],
    elapsed: Duration,
    rendered_frames: u32,
}

fn measure_candidates(
    clock: &dyn Clock,
    limit: Duration,
    frame: FrameConfig,
    workers: usize,
) -> Result<Measurements, CalibrationError> {
    let mut budget = ProbeBudget::new(clock, limit);
    let mut scene = ProbeScene::new(frame.viewport)?;
    budget.check()?;
    let mut samples = CANDIDATES.map(|(fine_tile, macro_tile)| CalibrationSample {
        fine_tile,
        macro_tile,
        elapsed_ns: [0; REPETITIONS],
        median_ns: 0,
    });
    let mut reference = Vec::new();
    for round in 0..REPETITIONS {
        reference.clear();
        let mut reference_fine = None;
        for slot in 0..CANDIDATES.len() {
            // Every candidate occupies every order position exactly once.
            // The first round starts with the declared baseline (16/128).
            let index = (round + slot + 1) % CANDIDATES.len();
            let (fine_tile, macro_tile) = CANDIDATES[index];
            budget.check()?;
            scene.set_pose([0.0, 0.0]);
            let mut renderer = RetainedFrameRenderer::new(RetainedFrameRendererConfig {
                frame,
                tiling: Tiling {
                    fine_tile,
                    macro_tile,
                },
                engine: EngineIdentity::fast(),
                threads: workers,
            })
            .map_err(probe_error)?;
            budget.check()?;
            budget.render(&mut renderer, &scene.stage, false)?;
            // Binary-fraction translations reset exactly between candidates.
            // Each frame changes actual retained placement, not camera serial.
            let pose = [
                [0.03125, 0.015625],
                [0.09375, -0.03125],
                [-0.0625, 0.046875],
            ][round];
            scene.set_pose(pose);
            samples[index].elapsed_ns[round] = budget.render(&mut renderer, &scene.stage, true)?;
            let cache = renderer.cache().stats();
            if renderer.raster_team_stats().frames_completed != 2
                || cache.misses.saturating_add(cache.poisoned) == 0
            {
                return Err(CalibrationError::Probe(
                    "the moving sample reused a complete frame instead of rasterizing".into(),
                ));
            }
            let candidate = renderer.frame();
            let comparison = if reference_fine.is_some() {
                reference.as_slice()
            } else {
                candidate.as_bytes()
            };
            if !pixels_equivalent(frame.viewport, comparison, candidate) {
                return Err(CalibrationError::PixelMismatch {
                    round: round + 1,
                    reference_fine_tile: reference_fine.unwrap_or(fine_tile),
                    candidate_fine_tile: fine_tile,
                });
            }
            if reference_fine.is_none() {
                reference.extend_from_slice(candidate.as_bytes());
                reference_fine = Some(fine_tile);
            }
            // Do not park a worker team for each candidate between rounds.
            drop(renderer);
            budget.check()?;
        }
    }
    for sample in &mut samples {
        let mut sorted = sample.elapsed_ns;
        sorted.sort_unstable();
        sample.median_ns = sorted[REPETITIONS / 2];
    }
    let elapsed = budget.check()?.saturating_sub(budget.started);
    Ok(Measurements {
        samples,
        elapsed,
        rendered_frames: budget.completed_frames,
    })
}

// Tile boundaries change the association of the fill kernel's sums. The
// renderer therefore promises numeric, not byte, equivalence across tiles.
// Reuse its versioned max/RMS linear-channel limits without claiming SSIM or
// certified equivalence. This fixed SDR probe also requires finite normalized
// values; even an invalid frame compared with itself must be refused.
fn pixels_equivalent(expected: Viewport, reference: &[u8], candidate: &FrameBuffer) -> bool {
    let Ok(layout) = FrameLayout::tight(PixelFormat::Rgba16F, expected.width, expected.height)
    else {
        return false;
    };
    if candidate.layout() != &layout || reference.len() != layout.total_bytes() {
        return false;
    }
    let mut squared_error = 0.0;
    for (reference, candidate) in reference
        .as_chunks::<2>()
        .0
        .iter()
        .zip(candidate.as_bytes().as_chunks::<2>().0.iter())
    {
        let reference =
            fmn_frame::half::f16_to_f64(u16::from_le_bytes([reference[0], reference[1]]));
        let candidate =
            fmn_frame::half::f16_to_f64(u16::from_le_bytes([candidate[0], candidate[1]]));
        if !reference.is_finite()
            || !candidate.is_finite()
            || !(0.0..=1.0).contains(&reference)
            || !(0.0..=1.0).contains(&candidate)
        {
            return false;
        }
        let error = (reference - candidate).abs();
        if error > PIXEL_EQUIVALENCE_MAX_CHANNEL_ERROR {
            return false;
        }
        squared_error += error * error;
    }
    // Comparing squared RMS avoids an unnecessary square root. The admitted
    // surface ceiling makes this channel count exactly representable in f64.
    let channels = f64::from(expected.width) * f64::from(expected.height) * 4.0;
    squared_error
        <= PIXEL_EQUIVALENCE_RMS_CHANNEL_ERROR * PIXEL_EQUIVALENCE_RMS_CHANNEL_ERROR * channels
}

fn select_candidate(samples: &[CalibrationSample; CANDIDATES.len()], baseline: usize) -> usize {
    let mut best = baseline;
    for (index, sample) in samples.iter().enumerate() {
        if sample.median_ns < samples[best].median_ns {
            best = index;
        }
    }
    if u128::from(samples[best].median_ns) * 100
        <= u128::from(samples[baseline].median_ns) * (100 - IMPROVEMENT_PERCENT)
    {
        best
    } else {
        baseline
    }
}

struct ProbeBudget<'a> {
    clock: &'a dyn Clock,
    started: Duration,
    last: Duration,
    limit: Duration,
    completed_frames: u32,
}

impl<'a> ProbeBudget<'a> {
    fn new(clock: &'a dyn Clock, limit: Duration) -> Self {
        let started = clock.monotonic();
        Self {
            clock,
            started,
            last: started,
            limit,
            completed_frames: 0,
        }
    }

    fn read(&mut self) -> Result<Duration, CalibrationError> {
        let now = self.clock.monotonic();
        if now < self.last {
            return Err(CalibrationError::ClockWentBackwards);
        }
        self.last = now;
        Ok(now)
    }

    fn within(&self, now: Duration) -> Result<(), CalibrationError> {
        let elapsed = now.saturating_sub(self.started);
        if elapsed >= self.limit {
            Err(CalibrationError::BudgetExceeded {
                budget: self.limit,
                elapsed,
                completed_frames: self.completed_frames,
            })
        } else {
            Ok(())
        }
    }

    fn check(&mut self) -> Result<Duration, CalibrationError> {
        let now = self.read()?;
        self.within(now)?;
        Ok(now)
    }

    fn render(
        &mut self,
        renderer: &mut RetainedFrameRenderer,
        stage: &Stage,
        measured: bool,
    ) -> Result<u64, CalibrationError> {
        let started = self.check()?;
        renderer.render(stage, 0).map_err(probe_error)?;
        self.completed_frames += 1;
        let finished = self.read()?;
        self.within(finished)?;
        if !measured {
            return Ok(0);
        }
        let nanos = u64::try_from(finished.saturating_sub(started).as_nanos())
            .map_err(|_| CalibrationError::UnmeasurableClock)?;
        if nanos == 0 {
            return Err(CalibrationError::UnmeasurableClock);
        }
        Ok(nanos)
    }
}

fn probe_error(error: impl fmt::Display) -> CalibrationError {
    CalibrationError::Probe(error.to_string())
}

/// Forty-eight ordinary primitives: sixteen quiet translucent back layers,
/// sixteen moving filled ellipses, and sixteen moving stroked cubic curves.
/// Geometry and painter order are fixed; only placement changes between poses.
struct ProbeScene {
    stage: Stage,
    moving: Vec<Mob>,
    pose: [f64; 2],
}

impl ProbeScene {
    fn new(viewport: Viewport) -> Result<Self, CalibrationError> {
        let mut stage = Stage::new();
        let mut moving = Vec::with_capacity(32);
        let width = f64::from(viewport.width) * 8.0 / f64::from(viewport.height);
        let cell_width = width / 4.0;
        let cell_height = 2.0;
        let palette = [
            Srgb::from_rgb8(55, 145, 225),
            Srgb::from_rgb8(245, 178, 69),
            Srgb::from_rgb8(174, 95, 212),
            Srgb::from_rgb8(69, 188, 157),
        ];
        let ink = Srgb::from_rgb8(238, 242, 248);
        for row in 0..4 {
            for column in 0..4 {
                let index = row * 4 + column;
                let x = -width / 2.0 + (f64::from(column) + 0.5) * cell_width;
                let y = -4.0 + (f64::from(row) + 0.5) * cell_height;
                let color = palette[index as usize % palette.len()];
                let back = stage.add(
                    Ellipse::new()
                        .width(cell_width * 1.14)
                        .height(cell_height * 1.12)
                        .arc_center([x, y, 0.0])
                        .color(color),
                );
                stage.set_fill(back, Some(color), Some(0.26), Some(0.0), true);
                stage.set_stroke(back, None, Some(0.0), Some(0.0), None, true);
                stage.set_z_index(back, -1, true);
                stage.add_to_scene(back).map_err(probe_error)?;

                let front = stage.add(
                    Ellipse::new()
                        .width(cell_width * 0.72)
                        .height(cell_height * 0.64)
                        .arc_center([x - cell_width * 0.06, y + 0.12, 0.0])
                        .color(color),
                );
                stage.set_fill(
                    front,
                    Some(color),
                    Some(0.43 + 0.08 * f64::from(index % 3)),
                    Some(0.0),
                    true,
                );
                stage.set_stroke(
                    front,
                    Some(ink),
                    Some(1.1 + 0.35 * f64::from(index % 3)),
                    Some(0.7),
                    None,
                    true,
                );
                stage.add_to_scene(front).map_err(probe_error)?;
                moving.push(front);

                let curve = CubicBezier::new(
                    [x - cell_width * 0.43, y - 0.40, 0.0],
                    [x + cell_width * 0.16, y + 1.12, 0.0],
                    [x - cell_width * 0.12, y - 1.08, 0.0],
                    [x + cell_width * 0.43, y + 0.38, 0.0],
                )
                .build()
                .map_err(probe_error)?;
                let curve = stage.add(curve);
                stage.set_fill(curve, None, Some(0.0), Some(0.0), true);
                stage.set_stroke(
                    curve,
                    Some(palette[(index as usize + 1) % palette.len()]),
                    Some(1.6 + 0.55 * f64::from(index % 3)),
                    Some(0.88),
                    None,
                    true,
                );
                stage.set_z_index(curve, 1, true);
                stage.add_to_scene(curve).map_err(probe_error)?;
                moving.push(curve);
            }
        }
        Ok(Self {
            stage,
            moving,
            pose: [0.0, 0.0],
        })
    }

    fn set_pose(&mut self, pose: [f64; 2]) {
        for (index, &mob) in self.moving.iter().enumerate() {
            let x_sign = if index % 2 == 0 { 1.0 } else { -1.0 };
            let y_sign = if index % 3 == 0 { -1.0 } else { 1.0 };
            self.stage.shift(
                mob,
                [
                    (pose[0] - self.pose[0]) * x_sign,
                    (pose[1] - self.pose[1]) * y_sign,
                    0.0,
                ],
            );
        }
        self.pose = pose;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fmn_config::config::DeterminismMode;
    use fmn_platform::clock::FakeClock;
    use fmn_platform::fs::VirtualFs;
    use fmn_runtime::{OutputPixelFormat, RenderIntent};
    use std::path::Path;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::SystemTime;

    #[cfg(windows)]
    const CACHE_ROOT: &str = r"C:\fmn-autotune-calibration-tests";
    #[cfg(not(windows))]
    const CACHE_ROOT: &str = "/fmn-autotune-calibration-tests";

    struct TickClock {
        ticks: AtomicU64,
        step: u64,
    }

    impl TickClock {
        fn new(step: u64) -> Self {
            Self {
                ticks: AtomicU64::new(0),
                step,
            }
        }
    }

    impl Clock for TickClock {
        fn monotonic(&self) -> Duration {
            Duration::from_nanos(self.ticks.fetch_add(self.step, Ordering::Relaxed))
        }

        fn wall(&self) -> SystemTime {
            SystemTime::UNIX_EPOCH
        }
    }

    struct UnusedClock;

    impl Clock for UnusedClock {
        fn monotonic(&self) -> Duration {
            panic!("this path must not measure or publish");
        }

        fn wall(&self) -> SystemTime {
            panic!("this path must not read wall time");
        }
    }

    fn fixture() -> (Arc<VirtualFs>, Config, PlanRequest, HardwareTopology) {
        let fs = Arc::new(VirtualFs::new());
        let mut config = Config::resolve(&[], None).unwrap().config;
        config.directories.cache = CACHE_ROOT.into();
        config.cache.max_bytes = 64 * 1024;
        config.camera.resolution = (64, 36);
        let topology = HardwareTopology::fallback(1);
        let request = PlanRequest::standard(
            RenderIntent::Preview,
            SurfaceSpec::lumen(64, 36),
            OutputPixelFormat::Rgba8,
        )
        .with_max_cpu_threads(1);
        (fs, config, request, topology)
    }

    fn small_frame() -> FrameConfig {
        FrameConfig::new(
            Viewport {
                width: 64,
                height: 36,
            },
            ScreenMap {
                scale: 4.5,
                origin: [32.0, 18.0],
                y_up: true,
            },
            LinearRgba {
                r: 0.018,
                g: 0.022,
                b: 0.030,
                a: 1.0,
            },
        )
    }

    fn solid_probe_frame(viewport: Viewport, rgba: [u16; 4]) -> FrameBuffer {
        let mut frame = FrameBuffer::new(
            FrameLayout::tight(PixelFormat::Rgba16F, viewport.width, viewport.height).unwrap(),
        );
        for pixel in frame.as_bytes_mut().as_chunks_mut::<8>().0 {
            for (channel, bits) in pixel.as_chunks_mut::<2>().0.iter_mut().zip(rgba) {
                channel.copy_from_slice(&bits.to_le_bytes());
            }
        }
        frame
    }

    #[test]
    fn pixel_policy_accepts_half_rounding_and_signed_zero() {
        let viewport = Viewport {
            width: 1,
            height: 1,
        };
        let reference = solid_probe_frame(viewport, [0x3800, 0x3400, 0x3a00, 0x3c00]);
        let rounded = solid_probe_frame(viewport, [0x3801, 0x3400, 0x3a00, 0x3c00]);
        assert_ne!(reference.as_bytes(), rounded.as_bytes());
        assert!(pixels_equivalent(viewport, reference.as_bytes(), &rounded));
        assert!(pixels_equivalent(viewport, rounded.as_bytes(), &reference));

        let positive = solid_probe_frame(viewport, [0x0000, 0x0000, 0x0000, 0x3c00]);
        let negative = solid_probe_frame(viewport, [0x8000, 0x0000, 0x0000, 0x3c00]);
        assert_ne!(positive.as_bytes(), negative.as_bytes());
        assert!(pixels_equivalent(viewport, positive.as_bytes(), &negative));
    }

    #[test]
    fn pixel_policy_rejects_shape_format_padding_and_truncated_reference() {
        let viewport = Viewport {
            width: 2,
            height: 1,
        };
        let reference = solid_probe_frame(viewport, [0x3800, 0x3800, 0x3800, 0x3c00]);
        let transposed = solid_probe_frame(
            Viewport {
                width: 1,
                height: 2,
            },
            [0x3800, 0x3800, 0x3800, 0x3c00],
        );
        assert_eq!(reference.as_bytes().len(), transposed.as_bytes().len());
        assert!(!pixels_equivalent(
            viewport,
            reference.as_bytes(),
            &transposed
        ));
        let other_format = FrameBuffer::new(FrameLayout::tight(PixelFormat::Rgba8, 4, 1).unwrap());
        assert_eq!(reference.as_bytes().len(), other_format.as_bytes().len());
        assert!(!pixels_equivalent(
            viewport,
            reference.as_bytes(),
            &other_format
        ));
        let padded = FrameBuffer::new(
            FrameLayout::with_row_alignment(PixelFormat::Rgba16F, 2, 1, 32).unwrap(),
        );
        assert!(!pixels_equivalent(viewport, reference.as_bytes(), &padded));
        assert!(!pixels_equivalent(
            viewport,
            &reference.as_bytes()[..15],
            &reference
        ));
        assert!(!pixels_equivalent(viewport, &[], &reference));
        assert!(!pixels_equivalent(
            Viewport {
                width: 0,
                height: 1
            },
            reference.as_bytes(),
            &reference,
        ));
    }

    #[test]
    fn pixel_policy_rejects_nonfinite_and_out_of_range_values_even_when_identical() {
        let viewport = Viewport {
            width: 1,
            height: 1,
        };
        let reference = solid_probe_frame(viewport, [0x3800, 0x3800, 0x3800, 0x3c00]);
        // +infinity, -infinity, NaN, negative subnormal, and just above one.
        for bits in [0x7c00, 0xfc00, 0x7e00, 0x8001, 0x3c01] {
            let invalid = solid_probe_frame(viewport, [bits, 0x3800, 0x3800, 0x3c00]);
            assert!(!pixels_equivalent(viewport, invalid.as_bytes(), &invalid));
            assert!(!pixels_equivalent(viewport, reference.as_bytes(), &invalid));
            assert!(!pixels_equivalent(viewport, invalid.as_bytes(), &reference));
        }
    }

    #[test]
    fn pixel_policy_rejects_a_local_error_even_when_rms_would_pass() {
        let viewport = Viewport {
            width: 128,
            height: 128,
        };
        let reference = solid_probe_frame(viewport, [0x3800, 0x3800, 0x3800, 0x3c00]);
        let mut local_error = reference.clone();
        // 0.59375 - 0.5 = 0.09375 exceeds the maximum 0.092, although this
        // single changed channel's RMS is 0.09375 / 256 = 0.0003662109375.
        local_error.as_bytes_mut()[..2].copy_from_slice(&0x38c0u16.to_le_bytes());
        const { assert!(0.09375 / 256.0 < PIXEL_EQUIVALENCE_RMS_CHANNEL_ERROR) };
        assert!(!pixels_equivalent(
            viewport,
            reference.as_bytes(),
            &local_error
        ));
    }

    #[test]
    fn pixel_policy_rejects_distributed_error_even_when_maximum_would_pass() {
        let viewport = Viewport {
            width: 4,
            height: 4,
        };
        let reference = solid_probe_frame(viewport, [0x3800, 0x3800, 0x3800, 0x3c00]);
        let shifted = solid_probe_frame(viewport, [0x3802, 0x3802, 0x3802, 0x3c00]);
        // Two half steps per RGB channel remain far below the maximum, but
        // a uniform colour shift exceeds the RMS limit.
        const { assert!(1.0 / 1024.0 < PIXEL_EQUIVALENCE_MAX_CHANNEL_ERROR) };
        assert!(!pixels_equivalent(viewport, reference.as_bytes(), &shifted));
    }

    #[test]
    fn pixel_policy_receipt_names_the_actual_renderer_bounds() {
        assert!(PIXEL_EQUIVALENCE_POLICY.contains(&format!(
            "max absolute error <= {}",
            PIXEL_EQUIVALENCE_MAX_CHANNEL_ERROR,
        )));
        assert!(PIXEL_EQUIVALENCE_POLICY.contains(&format!(
            "RMS error <= {}",
            PIXEL_EQUIVALENCE_RMS_CHANNEL_ERROR,
        )));
    }

    #[test]
    fn real_probe_changes_pixels_and_distinguishes_static_reuse() {
        let mut scene = ProbeScene::new(small_frame().viewport).unwrap();
        let mut renderer = RetainedFrameRenderer::new(RetainedFrameRendererConfig {
            frame: small_frame(),
            tiling: Tiling {
                fine_tile: 16,
                macro_tile: 128,
            },
            engine: EngineIdentity::fast(),
            threads: 1,
        })
        .unwrap();
        renderer.render(&scene.stage, 0).unwrap();
        let warm = renderer.frame().as_bytes().to_vec();
        scene.set_pose([0.09375, -0.03125]);
        renderer.render(&scene.stage, 0).unwrap();
        assert_ne!(renderer.frame().as_bytes(), warm.as_slice());
        assert_eq!(renderer.raster_team_stats().frames_completed, 2);
        assert!(renderer.cache().stats().misses > 0);
        renderer.render(&scene.stage, 0).unwrap();
        assert_eq!(
            renderer.raster_team_stats().frames_completed,
            2,
            "a static identity really would bypass the raster team",
        );
    }

    #[test]
    fn complete_real_probe_publishes_shared_profile_then_reuses_without_clock() {
        let (fs, config, request, topology) = fixture();
        let clock = Arc::new(TickClock::new(1_000));
        let report = calibrate(
            fs.clone(),
            clock,
            &config,
            request,
            &topology,
            Duration::from_secs(1),
        )
        .unwrap();
        assert_eq!(report.status, CalibrationStatus::New);
        assert_eq!(report.rendered_frames, 18);
        assert_eq!(report.workers, 1);
        assert_eq!(report.surface, request.surface());
        assert_eq!(report.samples.len(), 3);
        assert!(
            report
                .samples
                .iter()
                .all(|sample| sample.elapsed_ns.iter().all(|&n| n > 0))
        );
        assert_eq!(
            (report.profile.fine_tile, report.profile.macro_tile),
            (16, 128)
        );
        assert_eq!(
            load_profile(fs.as_ref(), &config, request, &topology).unwrap(),
            Some(report.profile),
        );
        let reused = calibrate(
            fs,
            Arc::new(UnusedClock),
            &config,
            request,
            &topology,
            Duration::ZERO,
        )
        .unwrap();
        assert_eq!(reused.status, CalibrationStatus::Reused);
        assert_eq!(reused.profile, report.profile);
        assert_eq!(reused.profile_key, report.profile_key);
        assert!(reused.samples.is_empty());
        assert_eq!(reused.rendered_frames, 0);
        assert_eq!(reused.elapsed, Duration::ZERO);
    }

    #[test]
    fn budget_exhaustion_after_real_frames_never_publishes_partial_timings() {
        let (fs, config, request, topology) = fixture();
        let error = calibrate(
            fs.clone(),
            Arc::new(TickClock::new(1_000_000)),
            &config,
            request,
            &topology,
            Duration::from_millis(12),
        )
        .unwrap_err();
        assert!(matches!(
            error,
            CalibrationError::BudgetExceeded { completed_frames, .. }
                if completed_frames > 0 && completed_frames < 18
        ));
        assert!(
            load_profile(fs.as_ref(), &config, request, &topology)
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn frozen_clock_does_not_invent_a_fastest_candidate() {
        let (fs, config, request, topology) = fixture();
        let error = calibrate(
            fs.clone(),
            Arc::new(FakeClock::new()),
            &config,
            request,
            &topology,
            Duration::from_secs(1),
        )
        .unwrap_err();
        assert!(matches!(error, CalibrationError::UnmeasurableClock));
        assert!(
            load_profile(fs.as_ref(), &config, request, &topology)
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn certified_refuses_foreign_cache_and_limits_refuse_before_clock() {
        let (fs, mut config, request, topology) = fixture();
        fs.insert(CACHE_ROOT, b"foreign data".to_vec());
        config.determinism.mode = DeterminismMode::Certified;
        let certified = PlanRequest::certified(
            RenderIntent::Preview,
            SurfaceSpec::lumen(64, 36),
            OutputPixelFormat::Rgba8,
        );
        let error = calibrate(
            fs.clone(),
            Arc::new(UnusedClock),
            &config,
            certified,
            &topology,
            Duration::from_secs(1),
        )
        .unwrap_err();
        assert!(matches!(error, CalibrationError::Unsupported(_)));
        assert_eq!(fs.read(Path::new(CACHE_ROOT)).unwrap(), b"foreign data");
        let fs = Arc::new(VirtualFs::new());
        config.determinism.mode = DeterminismMode::Standard;
        let large = PlanRequest::standard(
            RenderIntent::Preview,
            SurfaceSpec::lumen(1921, 1080),
            OutputPixelFormat::Rgba8,
        );
        let error = calibrate(
            fs.clone(),
            Arc::new(UnusedClock),
            &config,
            large,
            &topology,
            Duration::from_secs(1),
        )
        .unwrap_err();
        assert!(matches!(error, CalibrationError::Unsupported(_)));
        let error = calibrate(
            fs.clone(),
            Arc::new(UnusedClock),
            &config,
            request,
            &topology,
            Duration::ZERO,
        )
        .unwrap_err();
        assert!(matches!(error, CalibrationError::Unsupported(_)));
        assert!(
            load_profile(fs.as_ref(), &config, request, &topology)
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn selection_requires_median_improvement_instead_of_one_fast_outlier() {
        let sample = |index: usize, elapsed_ns: [u64; 3]| {
            let mut ordered = elapsed_ns;
            ordered.sort_unstable();
            CalibrationSample {
                fine_tile: CANDIDATES[index].0,
                macro_tile: CANDIDATES[index].1,
                elapsed_ns,
                median_ns: ordered[1],
            }
        };
        let mut rows = [
            sample(0, [1, 1000, 1000]),
            sample(1, [100, 100, 100]),
            sample(2, [99, 99, 99]),
        ];
        assert_eq!(select_candidate(&rows, 1), 1);
        rows[0] = sample(0, [94, 95, 96]);
        assert_eq!(select_candidate(&rows, 1), 0);
        rows[2] = sample(2, [80, 81, 79]);
        assert_eq!(select_candidate(&rows, 1), 2);
        rows[0] = sample(0, [100, 100, 100]);
        rows[2] = sample(2, [100, 100, 100]);
        assert_eq!(select_candidate(&rows, 1), 1);
    }

    #[test]
    fn immutable_conflict_reports_the_published_winner() {
        let (fs, config, request, topology) = fixture();
        let plan = ExecutionPlan::derive(request, &topology, None).unwrap();
        let offered = AutotuneProfile {
            topology: plan.topology_fingerprint,
            threads_per_render_team: 1,
            frames_in_flight: plan.frames_in_flight,
            fine_tile: 32,
            macro_tile: 256,
            scratch_bytes_per_worker: plan.render_teams[0].scratch_bytes_per_worker,
        };
        let incumbent = AutotuneProfile {
            fine_tile: 8,
            macro_tile: 64,
            ..offered
        };
        store_profile(
            fs.clone(),
            Arc::new(FakeClock::new()),
            &config,
            request,
            &topology,
            incumbent,
        )
        .unwrap();
        let identity = EngineIdentity::fast();
        let report = CalibrationReport {
            status: CalibrationStatus::New,
            profile_key: profile_key(&config, request, &topology).unwrap(),
            topology: plan.topology_fingerprint,
            surface: request.surface(),
            workers: 1,
            build_tier: identity.tier,
            renderer_version: identity.renderer_version,
            profile: offered,
            samples: vec![],
            elapsed: Duration::from_millis(5),
            rendered_frames: 18,
        };
        let winner = publish(
            fs.clone(),
            Arc::new(FakeClock::new()),
            &config,
            request,
            &topology,
            report,
        )
        .unwrap();
        assert_eq!(winner.status, CalibrationStatus::ConcurrentWinner);
        assert_eq!(winner.profile, incumbent);
        assert_eq!(
            load_profile(fs.as_ref(), &config, request, &topology).unwrap(),
            Some(incumbent),
        );
    }

    #[test]
    fn backwards_clock_and_plan_errors_retain_named_causes() {
        struct BackwardsClock(AtomicU64);
        impl Clock for BackwardsClock {
            fn monotonic(&self) -> Duration {
                Duration::from_nanos(10 - self.0.fetch_add(1, Ordering::Relaxed))
            }
            fn wall(&self) -> SystemTime {
                SystemTime::UNIX_EPOCH
            }
        }
        let clock = BackwardsClock(AtomicU64::new(0));
        let mut budget = ProbeBudget::new(&clock, Duration::from_secs(1));
        assert!(matches!(
            budget.check(),
            Err(CalibrationError::ClockWentBackwards)
        ));
        let (fs, config, request, topology) = fixture();
        let error = calibrate(
            fs,
            Arc::new(UnusedClock),
            &config,
            request.with_max_cpu_threads(0),
            &topology,
            Duration::from_secs(1),
        )
        .unwrap_err();
        assert!(matches!(
            error,
            CalibrationError::Plan(PlanError::ZeroLimit(_))
        ));
        assert!(std::error::Error::source(&error).is_some());
    }
}
