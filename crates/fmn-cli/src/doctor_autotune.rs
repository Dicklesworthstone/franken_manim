//! Explicit doctor entry point for the shared raster-tile calibration.
//!
//! The request builder is shared with ordinary rendering. The receipt names
//! the limited measurement scope and keeps this attempt's timings separate
//! from a reused or concurrently published immutable profile.

use std::fmt::Write as _;
use std::sync::Arc;
use std::time::Duration;

use fmn::autotune::AutotuneError;
use fmn::autotune::calibration::{
    CALIBRATION_WORKLOAD, CalibrationError, CalibrationReport, CalibrationStatus,
    PIXEL_EQUIVALENCE_POLICY, calibrate,
};
use fmn_cache::{CacheError, CacheRootError};
use fmn_config::config::{DeterminismMode, Engine};
use fmn_platform::clock::{Clock, StdClock};
use fmn_platform::fs::FileSystem;
use fmn_platform::topology::HardwareTopology;
use fmn_runtime::{ExecutionPlan, PlanError, RenderIntent};

use super::{
    CliError, DoctorCommand, MetalSelection, ROBOT_SCHEMA_VERSION, TopologySource,
    check_simd_tier_support, detect_topology, execution_plan_error, execution_plan_request,
    json_option, json_string, planning_output_format, resolve_common_config,
};

const PROBE_BUDGET: Duration = Duration::from_secs(10);
const PROBE_NOTICE: &str = "This profile concerns raster tile sizes for one retained 2D renderer \
    team. Team counts, frame slots and scratch policy remain topology/request-derived. \
    The probe does not establish end-to-end throughput or a guaranteed speedup. \
    Budget checks occur between bounded frames, so one in-flight frame may overrun.";

/// Run the explicitly requested probe and return a prefix for the doctor report.
pub(crate) fn run(fs: Arc<dyn FileSystem>, command: &DoctorCommand) -> Result<String, CliError> {
    run_with_clock_factory(fs, command, || Arc::new(StdClock::new()))
}

// A factory keeps even host-clock construction beyond certified/annex refusal.
// Tests inject deterministic clock readings while using the actual renderer,
// request builder, configuration resolver and immutable cache.
fn run_with_clock_factory(
    fs: Arc<dyn FileSystem>,
    command: &DoctorCommand,
    clock: impl FnOnce() -> Arc<dyn Clock>,
) -> Result<String, CliError> {
    if !command.autotune {
        return Ok(String::new());
    }
    let config = resolve_common_config(fs.as_ref(), &command.common)?;
    if config.determinism.mode != DeterminismMode::Standard {
        return Err(CliError::new(
            "config",
            "doctor --autotune requires standard determinism; certified rendering never measures or consults raster tuning profiles",
        ));
    }
    if config.render.engine != Engine::Cpu {
        return Err(CliError::new(
            "capability",
            "doctor --autotune measures only the standard CPU raster renderer; select render.engine=cpu",
        ));
    }
    check_simd_tier_support()?;
    let output_format = planning_output_format(&config.file_writer.pixel_format)?;
    let request = execution_plan_request(
        &config,
        RenderIntent::Offline,
        output_format,
        MetalSelection::Offline,
        false,
    )?;
    let (topology, source) = detect_topology(fs.as_ref());
    let baseline = ExecutionPlan::derive(request, &topology, None).map_err(execution_plan_error)?;
    let report = calibrate(fs, clock(), &config, request, &topology, PROBE_BUDGET)
        .map_err(calibration_error)?;
    Ok(if command.common.robot {
        receipt_ndjson(&report, &baseline, &topology, &source)
    } else {
        receipt_human(&report, &baseline, &topology, &source)
    })
}

fn calibration_error(error: CalibrationError) -> CliError {
    let exit_name = match &error {
        CalibrationError::BudgetExceeded { .. } => "budget",
        CalibrationError::Plan(error) => execution_plan_error(error.clone()).exit_name(),
        CalibrationError::Autotune(AutotuneError::Plan(PlanError::InvalidAutotune)) => "capability",
        CalibrationError::Autotune(AutotuneError::Plan(error)) => {
            execution_plan_error(error.clone()).exit_name()
        }
        CalibrationError::Autotune(AutotuneError::Cache(CacheError::RootResolution(
            CacheRootError::InvalidConfigured { .. },
        ))) => "config",
        CalibrationError::Autotune(AutotuneError::Cache(
            CacheError::InvalidNamespace { .. }
            | CacheError::EntryTooLarge { .. }
            | CacheError::Encode(_),
        )) => "internal",
        CalibrationError::Probe(_) | CalibrationError::PixelMismatch { .. } => "render",
        CalibrationError::Unsupported(_)
        | CalibrationError::Autotune(_)
        | CalibrationError::ClockWentBackwards
        | CalibrationError::UnmeasurableClock
        | CalibrationError::ConcurrentWinnerUnavailable => "capability",
    };
    CliError::new(exit_name, format!("doctor --autotune: {error}"))
}

fn source_description(source: &TopologySource) -> (&'static str, Option<&str>) {
    match source {
        TopologySource::LinuxSysfs => ("linux-sysfs", None),
        TopologySource::Fallback { reason } => ("fallback", Some(reason.as_str())),
    }
}

fn measurement_notice(status: CalibrationStatus) -> &'static str {
    match status {
        CalibrationStatus::New => {
            "A complete measurement was accepted by the immutable store; samples describe this attempt."
        }
        CalibrationStatus::Reused => {
            "An existing immutable profile was reused; no new measurement was made."
        }
        CalibrationStatus::ConcurrentWinner => {
            "A concurrent immutable profile won publication; samples describe this attempt, not the winner's original measurements."
        }
    }
}

fn receipt_ndjson(
    report: &CalibrationReport,
    baseline: &ExecutionPlan,
    topology: &HardwareTopology,
    source: &TopologySource,
) -> String {
    let mut out = String::new();
    let (source_name, source_detail) = source_description(source);
    let _ = write!(
        out,
        "{{\"schema\":\"fmn.autotune\",\"version\":{},\"kind\":\"calibration\",\
         \"status\":{},\"scope\":\"raster-tiles-only\",\"intent\":\"offline\",\
         \"profile_key\":{},\"workload\":{},\"pixel_equivalence\":{},",
        ROBOT_SCHEMA_VERSION,
        json_string(report.status.as_str()),
        json_string(&report.profile_key.digest().to_hex()),
        json_string(CALIBRATION_WORKLOAD),
        json_string(PIXEL_EQUIVALENCE_POLICY),
    );
    let _ = write!(
        out,
        "\"topology\":{{\"fingerprint\":{},\"source\":{},\"source_detail\":{},\
         \"logical_cores\":{},\"physical_cores\":{}}},\
         \"width\":{},\"height\":{},\"workers\":{},\
         \"compiled_tier\":{},\"renderer_version\":{},",
        json_string(&format!("{:016x}", report.topology.value())),
        json_string(source_name),
        json_option(source_detail),
        topology.cpus.len(),
        topology.physical_cores,
        report.surface.width,
        report.surface.height,
        report.workers,
        json_string(report.build_tier.name()),
        report.renderer_version,
    );
    let _ = write!(
        out,
        "\"baseline\":{{\"fine_tile\":{},\"macro_tile\":{}}},\
         \"selected\":{{\"fine_tile\":{},\"macro_tile\":{}}},\
         \"probe_budget_ms\":{},\"probe_elapsed_ns\":{},\"rendered_frames\":{},\
         \"sample_origin\":\"this-attempt\",\"samples\":[",
        baseline.fine_tile,
        baseline.macro_tile,
        report.profile.fine_tile,
        report.profile.macro_tile,
        PROBE_BUDGET.as_millis(),
        report.elapsed.as_nanos(),
        report.rendered_frames,
    );
    for (index, sample) in report.samples.iter().enumerate() {
        if index != 0 {
            out.push(',');
        }
        let _ = write!(
            out,
            "{{\"fine_tile\":{},\"macro_tile\":{},\"elapsed_ns\":[{},{},{}],\"median_ns\":{}}}",
            sample.fine_tile,
            sample.macro_tile,
            sample.elapsed_ns[0],
            sample.elapsed_ns[1],
            sample.elapsed_ns[2],
            sample.median_ns,
        );
    }
    let _ = writeln!(
        out,
        "],\"measurement_note\":{},\"warning\":{}}}",
        json_string(measurement_notice(report.status)),
        json_string(PROBE_NOTICE),
    );
    out
}

fn receipt_human(
    report: &CalibrationReport,
    baseline: &ExecutionPlan,
    topology: &HardwareTopology,
    source: &TopologySource,
) -> String {
    let mut out = String::new();
    let (source_name, source_detail) = source_description(source);
    let _ = writeln!(
        out,
        "Raster tile calibration: {}\n\
         Profile: {}\n\
         Workload: {CALIBRATION_WORKLOAD}\n\
         Pixel check: {PIXEL_EQUIVALENCE_POLICY}\n\
         Topology: {:016x} ({source_name}; {} logical, {} physical cores)\n\
         Surface: {}x{} pixels; workers: {}\n\
         Compiled tier: {}; renderer version: {}\n\
         Baseline tiles: fine {}, macro {}\n\
         Selected profile tiles: fine {}, macro {}\n\
         Probe: {} completed frames; elapsed {} ns; budget {} ms",
        report.status.as_str(),
        report.profile_key.digest().to_hex(),
        report.topology.value(),
        topology.cpus.len(),
        topology.physical_cores,
        report.surface.width,
        report.surface.height,
        report.workers,
        report.build_tier.name(),
        report.renderer_version,
        baseline.fine_tile,
        baseline.macro_tile,
        report.profile.fine_tile,
        report.profile.macro_tile,
        report.rendered_frames,
        report.elapsed.as_nanos(),
        PROBE_BUDGET.as_millis(),
    );
    if let Some(detail) = source_detail {
        let _ = writeln!(out, "Topology detail: {detail}");
    }
    if !report.samples.is_empty() {
        out.push_str("Moving-frame timings from this attempt:\n");
        for sample in &report.samples {
            let _ = writeln!(
                out,
                "  fine {}, macro {}: [{}, {}, {}] ns; median {} ns",
                sample.fine_tile,
                sample.macro_tile,
                sample.elapsed_ns[0],
                sample.elapsed_ns[1],
                sample.elapsed_ns[2],
                sample.median_ns,
            );
        }
    }
    let _ = writeln!(out, "{}", measurement_notice(report.status));
    let _ = writeln!(out, "Scope: {PROBE_NOTICE}\n");
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use fmn_platform::fs::VirtualFs;
    use std::path::Path;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::SystemTime;

    #[cfg(windows)]
    const CACHE_ROOT: &str = r"C:\fmn-doctor-autotune-tests";
    #[cfg(not(windows))]
    const CACHE_ROOT: &str = "/fmn-doctor-autotune-tests";

    struct TickClock(AtomicU64);

    impl Clock for TickClock {
        fn monotonic(&self) -> Duration {
            Duration::from_nanos(self.0.fetch_add(1_000, Ordering::Relaxed))
        }

        fn wall(&self) -> SystemTime {
            SystemTime::UNIX_EPOCH
        }
    }

    struct UnusedClock;

    impl Clock for UnusedClock {
        fn monotonic(&self) -> Duration {
            panic!("cached receipt must not measure");
        }

        fn wall(&self) -> SystemTime {
            panic!("cached receipt must not publish");
        }
    }

    fn fixture() -> (Arc<VirtualFs>, DoctorCommand) {
        let fs = Arc::new(VirtualFs::new());
        fs.insert("/sys/devices/system/cpu/online", b"0\n".to_vec());
        fs.insert(
            "/proc/meminfo",
            b"MemTotal: 4194304 kB\nMemAvailable: 4194304 kB\n".to_vec(),
        );
        fs.insert(
            "doctor-autotune.yml",
            b"camera:\n  resolution: \"(64, 36)\"\ncache:\n  max_bytes: 65536\n".to_vec(),
        );
        let invocation = super::super::parse_args([
            "doctor",
            "--autotune",
            "--robot",
            "--threads=1",
            "--cache-dir",
            CACHE_ROOT,
            "--config_file",
            "doctor-autotune.yml",
        ])
        .unwrap();
        let super::super::Invocation::Doctor(command) = invocation else {
            panic!("doctor command expected");
        };
        (fs, command)
    }

    #[test]
    fn tiny_real_probe_and_cached_receipts_use_one_shared_profile() {
        let (fs, mut command) = fixture();
        let fresh = run_with_clock_factory(fs.clone(), &command, || {
            Arc::new(TickClock(AtomicU64::new(0)))
        })
        .unwrap();
        assert_eq!(fresh.lines().count(), 1);
        assert!(fresh.starts_with("{\"schema\":\"fmn.autotune\",\"version\":1,"));
        assert!(fresh.ends_with("}\n"));
        assert!(fresh.contains("\"status\":\"new\""));
        assert!(fresh.contains("\"scope\":\"raster-tiles-only\""));
        assert!(fresh.contains("\"width\":64,\"height\":36,\"workers\":1"));
        assert!(fresh.contains("\"rendered_frames\":18"));
        assert!(fresh.contains("\"selected\":{\"fine_tile\":16,\"macro_tile\":128}"));
        assert_eq!(fresh.matches("\"median_ns\":").count(), 3);
        assert!(fresh.contains(PROBE_NOTICE));
        assert!(fresh.contains(&format!(
            "\"pixel_equivalence\":{}",
            json_string(PIXEL_EQUIVALENCE_POLICY),
        )));

        let config = resolve_common_config(fs.as_ref(), &command.common).unwrap();
        let output_format = planning_output_format(&config.file_writer.pixel_format).unwrap();
        let request = execution_plan_request(
            &config,
            RenderIntent::Offline,
            output_format,
            MetalSelection::Offline,
            false,
        )
        .unwrap();
        let (topology, _) = detect_topology(fs.as_ref());
        let profile = fmn::autotune::load_profile(fs.as_ref(), &config, request, &topology)
            .unwrap()
            .unwrap();
        let planned =
            fmn::autotune::derive_execution_plan(fs.as_ref(), &config, request, &topology).unwrap();
        assert_eq!(
            (planned.fine_tile, planned.macro_tile),
            (profile.fine_tile, profile.macro_tile)
        );
        let key = fmn::autotune::profile_key(&config, request, &topology)
            .unwrap()
            .digest()
            .to_hex();
        assert!(fresh.contains(&format!("\"profile_key\":\"{key}\"")));

        let reused =
            run_with_clock_factory(fs.clone(), &command, || Arc::new(UnusedClock)).unwrap();
        assert!(reused.contains("\"status\":\"reused\""));
        assert!(reused.contains("\"probe_elapsed_ns\":0,\"rendered_frames\":0"));
        assert!(reused.contains("\"samples\":[]"));
        assert!(reused.contains(&format!("\"profile_key\":\"{key}\"")));

        command.common.robot = false;
        let human = run(fs, &command).unwrap();
        assert!(human.starts_with("Raster tile calibration: reused\n"));
        assert!(human.contains("Surface: 64x36 pixels; workers: 1"));
        assert!(human.contains("no new measurement was made"));
        assert!(human.contains(PROBE_NOTICE));
        assert!(human.contains(PIXEL_EQUIVALENCE_POLICY));
    }

    #[test]
    fn reproducible_refusal_precedes_clock_and_foreign_cache_access() {
        let (fs, mut command) = fixture();
        command.common.reproducible = true;
        fs.insert(CACHE_ROOT, b"foreign data".to_vec());
        let error = run_with_clock_factory(fs.clone(), &command, || {
            panic!("certified refusal must precede clock construction")
        })
        .unwrap_err();
        assert_eq!(error.exit_name(), "config");
        assert!(error.message().contains("requires standard determinism"));
        assert_eq!(fs.read(Path::new(CACHE_ROOT)).unwrap(), b"foreign data");
    }

    #[test]
    fn foreign_cache_is_an_explicit_capability_refusal() {
        let (fs, command) = fixture();
        fs.insert(CACHE_ROOT, b"foreign data".to_vec());
        let error =
            run_with_clock_factory(fs.clone(), &command, || Arc::new(UnusedClock)).unwrap_err();
        assert_eq!(error.exit_name(), "capability");
        assert!(error.message().contains("doctor --autotune:"));
        assert!(error.message().contains("cache"));
        assert_eq!(fs.read(Path::new(CACHE_ROOT)).unwrap(), b"foreign data");
    }

    #[test]
    fn typed_refusals_keep_the_cli_budget_config_and_render_categories() {
        let budget = calibration_error(CalibrationError::BudgetExceeded {
            budget: Duration::from_secs(10),
            elapsed: Duration::from_secs(11),
            completed_frames: 3,
        });
        assert_eq!(budget.exit_name(), "budget");
        assert!(
            budget
                .message()
                .contains("this attempt published no profile")
        );
        let config = calibration_error(CalibrationError::Plan(PlanError::ZeroLimit("threads")));
        assert_eq!(config.exit_name(), "config");
        let raster = calibration_error(CalibrationError::PixelMismatch {
            round: 1,
            reference_fine_tile: 16,
            candidate_fine_tile: 8,
        });
        assert_eq!(raster.exit_name(), "render");
        let cache = calibration_error(CalibrationError::Autotune(AutotuneError::Cache(
            CacheError::FormatUnsupported {
                found: "unknown".into(),
            },
        )));
        assert_eq!(cache.exit_name(), "capability");
        assert!(cache.message().contains("unsupported cache store format"));
    }
}
