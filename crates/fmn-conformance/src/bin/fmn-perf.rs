//! Governed robot front door for performance policy, evidence, and producers.

#![forbid(unsafe_code)]

use fmn_cache::{Store, StoreConfig};
use fmn_conformance::perf::{
    BASELINE_SCHEMA, Baseline, EvidenceKind, EvidenceRef, MeasurementBatch, POLICY_SCHEMA,
    SAMPLES_SCHEMA, parse_policy_catalog, render_policy_catalog, require_compiled_cargo_profile,
};
use fmn_conformance::perf_pg2::{
    PG2_DEFINITION_SCHEMA, PG2_SAMPLE_COUNT, PG2_THREADS, PG2_WARMUP_ITERATIONS, Pg2Definition,
    Pg2Scenario, measure_pg2,
};
use fmn_conformance::perf_pg7::{
    PG7_DEFINITION_SCHEMA, PG7_SAMPLE_COUNT, PG7_WARMUP_ITERATIONS, Pg7Definition, Pg7Scenario,
    measure_pg7,
};
use fmn_hash::sha256;
use fmn_platform::clock::StdClock;
use fmn_platform::fs::StdFs;
use std::ffi::OsStr;
use std::fmt::Write as _;
use std::fs;
use std::fs::OpenOptions;
use std::io::{Read as _, Write as _};
use std::path::{Component, Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;

const CLI_SCHEMA: &str = "fmn-perf-cli/1";
const EXIT_OK: u8 = 0;
const EXIT_USAGE: u8 = 64;
const EXIT_DATA: u8 = 65;
const EXIT_IO: u8 = 74;
const MAX_POLICY_BYTES: u64 = 1024 * 1024;
const MAX_BASELINE_BYTES: u64 = 64 * 1024;
const MAX_RAW_BYTES: u64 = 128 * 1024 * 1024;

fn main() -> ExitCode {
    let arguments: Vec<_> = std::env::args_os().skip(1).collect();
    let (output, code) = match dispatch(&arguments) {
        Ok(output) => (output, EXIT_OK),
        Err(error) => (format!("{}\n", error.to_ndjson()), error.exit_code),
    };
    if std::io::stdout().write_all(output.as_bytes()).is_err() {
        ExitCode::from(EXIT_IO)
    } else {
        ExitCode::from(code)
    }
}

fn dispatch(arguments: &[std::ffi::OsString]) -> Result<String, CliError> {
    let Some(command) = arguments.first().and_then(|value| value.to_str()) else {
        return Err(CliError::usage(
            "expected catalog, verify-baseline, pg2-definitions, measure-pg2, \
             pg7-definitions, or measure-pg7",
        ));
    };
    match command {
        "catalog" if arguments.len() == 2 => catalog(
            arguments
                .get(1)
                .ok_or_else(|| CliError::usage("missing path"))?,
        ),
        "verify-baseline" if arguments.len() == 2 => verify_baseline(
            arguments
                .get(1)
                .ok_or_else(|| CliError::usage("missing path"))?,
        ),
        "pg2-definitions" if arguments.len() == 1 => Ok(pg2_definitions()),
        "pg7-definitions" if arguments.len() == 1 => pg7_definitions(),
        "measure-pg2" if arguments.len() == 5 => measure_pg2_command(
            arguments
                .get(1)
                .ok_or_else(|| CliError::usage("missing baseline path"))?,
            arguments
                .get(2)
                .ok_or_else(|| CliError::usage("missing producer commit"))?,
            arguments
                .get(3)
                .ok_or_else(|| CliError::usage("missing trace output path"))?,
            arguments
                .get(4)
                .ok_or_else(|| CliError::usage("missing raw output path"))?,
        ),
        "measure-pg7" if arguments.len() == 6 => measure_pg7_command(
            arguments
                .get(1)
                .ok_or_else(|| CliError::usage("missing baseline path"))?,
            arguments
                .get(2)
                .ok_or_else(|| CliError::usage("missing producer commit"))?,
            arguments
                .get(3)
                .ok_or_else(|| CliError::usage("missing cache root"))?,
            arguments
                .get(4)
                .ok_or_else(|| CliError::usage("missing trace output path"))?,
            arguments
                .get(5)
                .ok_or_else(|| CliError::usage("missing raw output path"))?,
        ),
        "catalog" | "verify-baseline" => Err(CliError::usage(format!(
            "{command} requires exactly one path argument"
        ))),
        "pg2-definitions" => Err(CliError::usage("pg2-definitions does not accept arguments")),
        "pg7-definitions" => Err(CliError::usage("pg7-definitions does not accept arguments")),
        "measure-pg2" => Err(CliError::usage(
            "measure-pg2 requires <baseline.tsv> <producer-commit> <trace.tsv> <raw.tsv>",
        )),
        "measure-pg7" => Err(CliError::usage(
            "measure-pg7 requires <baseline.tsv> <producer-commit> \
             <cache-root-or-dash> <trace.tsv> <raw.tsv>",
        )),
        _ => Err(CliError::usage(format!("unknown command {command:?}"))),
    }
}

fn catalog(path: &OsStr) -> Result<String, CliError> {
    let text = read_utf8(path, "policy catalog", MAX_POLICY_BYTES)?;
    let mut policies =
        parse_policy_catalog(&text).map_err(|error| CliError::data(error.to_string()))?;
    policies.sort_by(|left, right| {
        (left.gate, left.scenario.as_str()).cmp(&(right.gate, right.scenario.as_str()))
    });
    let canonical = render_policy_catalog(&policies);
    let mut output = String::new();
    let _ = writeln!(
        output,
        "{{\"schema\":\"{CLI_SCHEMA}\",\"kind\":\"catalog\",\
         \"policy_schema\":\"{POLICY_SCHEMA}\",\"policy_count\":{},\
         \"canonical_digest\":\"{}\"}}",
        policies.len(),
        sha256(canonical.as_bytes()),
    );
    for policy in policies {
        let _ = write!(
            output,
            "{{\"schema\":\"{CLI_SCHEMA}\",\"kind\":\"policy\",\
             \"gate\":\"{}\",\"scenario\":\"{}\",\"unit\":\"{}\",\
             \"direction\":\"{}\",\"target\":",
            policy.gate,
            escape_json(&policy.scenario),
            policy.unit.name(),
            policy.direction.name(),
        );
        if let Some(target) = policy.target {
            let _ = write!(output, "{target}");
        } else {
            output.push_str("null");
        }
        let _ = writeln!(
            output,
            ",\"min_valid_samples\":{},\"max_invalid_samples\":{},\
             \"max_mad_bps\":{},\"alert_regression_bps\":{},\
             \"block_regression_bps\":{},\"enforcement\":\"{}\",\
             \"scope\":\"{}\",\"require_regression_profile\":{}}}",
            policy.min_valid_samples,
            policy.max_invalid_samples,
            policy.max_mad_bps,
            policy.alert_regression_bps,
            policy.block_regression_bps,
            policy.enforcement.name(),
            policy.scope.name(),
            policy.require_regression_profile,
        );
    }
    Ok(output)
}

fn verify_baseline(path: &OsStr) -> Result<String, CliError> {
    let text = read_utf8(path, "baseline", MAX_BASELINE_BYTES)?;
    let baseline = Baseline::from_tsv(&text).map_err(|error| CliError::data(error.to_string()))?;
    let observation = baseline
        .observation
        .as_ref()
        .ok_or_else(|| CliError::data("target-only baseline has no raw observation to verify"))?;
    let raw = read_utf8(
        OsStr::new(&observation.source.path),
        "raw observation",
        MAX_RAW_BYTES,
    )?;
    let batch =
        MeasurementBatch::from_tsv(&raw).map_err(|error| CliError::data(error.to_string()))?;
    baseline
        .verify_observation_batch(&batch)
        .map_err(|error| CliError::data(error.to_string()))?;
    Ok(format!(
        "{{\"schema\":\"{CLI_SCHEMA}\",\"kind\":\"baseline-verification\",\
         \"baseline_schema\":\"{BASELINE_SCHEMA}\",\
         \"sample_schema\":\"{SAMPLES_SCHEMA}\",\"gate\":\"{}\",\
         \"scenario\":\"{}\",\"generation\":{},\"producer_commit\":\"{}\",\
         \"source_path\":\"{}\",\"source_digest\":\"{}\",\"status\":\"verified\"}}\n",
        baseline.policy.gate,
        escape_json(&baseline.policy.scenario),
        baseline.generation,
        baseline.producer_commit,
        escape_json(&observation.source.path),
        observation.source.digest,
    ))
}

fn pg2_definitions() -> String {
    let mut output = String::new();
    for scenario in Pg2Scenario::ALL {
        let definition = Pg2Definition::new(scenario);
        let _ = writeln!(
            output,
            "{{\"schema\":\"{CLI_SCHEMA}\",\"kind\":\"pg2-definition\",\
             \"definition_schema\":\"{PG2_DEFINITION_SCHEMA}\",\"gate\":\"pg-2\",\
             \"scenario\":\"{}\",\"benchmark_definition\":\"{}\",\
             \"config_digest\":\"{}\",\"expected_frame_digest\":\"{}\",\
             \"engine\":\"fast-cpu\",\"tier\":\"{}\",\
             \"thread_profile\":\"fixed-8\",\"threads\":{PG2_THREADS},\
             \"sample_count\":{PG2_SAMPLE_COUNT},\
             \"warmup_iterations\":{PG2_WARMUP_ITERATIONS},\
             \"iterations_per_sample\":{},\"work_units_per_iteration\":{}}}",
            scenario.name(),
            definition.digest(),
            definition.config_digest(),
            definition.expected_frame_digest(),
            fmn_render::Tier::COMPILED.name(),
            definition.iterations_per_sample(),
            definition.work_units_per_iteration(),
        );
    }
    output
}

fn pg7_definitions() -> Result<String, CliError> {
    let mut output = String::new();
    for scenario in Pg7Scenario::ALL {
        let definition =
            Pg7Definition::new(scenario).map_err(|error| CliError::data(error.to_string()))?;
        let _ = writeln!(
            output,
            "{{\"schema\":\"{CLI_SCHEMA}\",\"kind\":\"pg7-definition\",\
             \"definition_schema\":\"{PG7_DEFINITION_SCHEMA}\",\"gate\":\"pg-7\",\
             \"scenario\":\"{}\",\"benchmark_definition\":\"{}\",\
             \"config_digest\":\"{}\",\"fixture_input_digest\":\"{}\",\
             \"expected_result_digest\":\"{}\",\"engine\":\"{}\",\"tier\":\"portable\",\
             \"thread_profile\":\"single-thread\",\"cache_state\":\"{}\",\
             \"output_mode\":\"{}\",\"sample_count\":{PG7_SAMPLE_COUNT},\
             \"warmup_iterations\":{PG7_WARMUP_ITERATIONS}}}",
            scenario.name(),
            definition.digest(),
            definition.config_digest(),
            definition.fixture_input_digest(),
            definition.expected_result_digest(),
            scenario.engine(),
            scenario.cache_state(),
            scenario.output_mode(),
        );
    }
    Ok(output)
}

fn measure_pg2_command(
    baseline_path: &OsStr,
    producer_commit: &OsStr,
    trace_path: &OsStr,
    raw_path: &OsStr,
) -> Result<String, CliError> {
    let baseline_text = read_utf8(baseline_path, "baseline", MAX_BASELINE_BYTES)?;
    let baseline =
        Baseline::from_tsv(&baseline_text).map_err(|error| CliError::data(error.to_string()))?;
    let producer_commit = utf8_argument(producer_commit, "producer commit")?;
    let trace_path_text = utf8_argument(trace_path, "trace output path")?;
    let raw_path_text = utf8_argument(raw_path, "raw output path")?;
    if trace_path_text == raw_path_text {
        return Err(CliError::data(
            "trace and raw output paths must be distinct",
        ));
    }
    // Validate both repository artifact paths and refuse replacement before
    // the expensive render. The trace is published first, so a later raw-file
    // race can leave only a clearly incomplete trace, never a plausible bundle
    // naming bytes that were not written.
    EvidenceRef::from_bytes(EvidenceKind::PhaseTrace, trace_path_text, &[])
        .map_err(|error| CliError::data(error.to_string()))?;
    EvidenceRef::from_bytes(EvidenceKind::RawSamples, raw_path_text, &[])
        .map_err(|error| CliError::data(error.to_string()))?;
    validate_output_parent(trace_path, "trace output")?;
    validate_output_parent(raw_path, "raw output")?;
    refuse_existing(trace_path, "trace output")?;
    refuse_existing(raw_path, "raw output")?;

    let artifacts = measure_pg2(&baseline, producer_commit, trace_path_text)
        .map_err(|error| CliError::data(error.to_string()))?;
    let raw = artifacts
        .batch
        .to_tsv()
        .map_err(|error| CliError::data(error.to_string()))?;
    let raw_digest = sha256(raw.as_bytes());
    let trace_digest = sha256(artifacts.trace_tsv.as_bytes());
    let valid_samples = artifacts
        .batch
        .samples
        .iter()
        .filter(|sample| sample.invalid_reason.is_none())
        .count();
    let invalid_samples = artifacts.batch.samples.len() - valid_samples;

    write_new(trace_path, artifacts.trace_tsv.as_bytes(), "trace output")?;
    if let Err(error) = write_new(raw_path, raw.as_bytes(), "raw output") {
        return Err(CliError::io(format!(
            "{}; trace output {trace_path_text:?} was already published and was not deleted",
            error.detail
        )));
    }

    Ok(format!(
        "{{\"schema\":\"{CLI_SCHEMA}\",\"kind\":\"pg2-measurement\",\
         \"gate\":\"pg-2\",\"scenario\":\"{}\",\"benchmark_definition\":\"{}\",\
         \"config_digest\":\"{}\",\"producer_commit\":\"{}\",\
         \"sample_count\":{},\"valid_samples\":{},\"invalid_samples\":{},\
         \"bare_metal\":{},\"isolation_qualified\":{},\
         \"frame_digest\":\"{}\",\"trace_path\":\"{}\",\
         \"trace_digest\":\"{}\",\"raw_path\":\"{}\",\"raw_digest\":\"{}\",\
         \"status\":\"measured-not-evaluated\"}}\n",
        escape_json(&baseline.policy.scenario),
        artifacts.batch.key.benchmark_definition,
        artifacts.batch.key.config_digest,
        producer_commit,
        artifacts.batch.samples.len(),
        valid_samples,
        invalid_samples,
        artifacts.batch.key.bare_metal,
        artifacts.batch.key.isolated,
        artifacts.frame_digest,
        escape_json(trace_path_text),
        trace_digest,
        escape_json(raw_path_text),
        raw_digest,
    ))
}

fn measure_pg7_command(
    baseline_path: &OsStr,
    producer_commit: &OsStr,
    cache_root: &OsStr,
    trace_path: &OsStr,
    raw_path: &OsStr,
) -> Result<String, CliError> {
    let baseline_text = read_utf8(baseline_path, "baseline", MAX_BASELINE_BYTES)?;
    let baseline =
        Baseline::from_tsv(&baseline_text).map_err(|error| CliError::data(error.to_string()))?;
    let scenario = Pg7Scenario::parse(&baseline.policy.scenario).ok_or_else(|| {
        CliError::data(format!(
            "unsupported PG-7 scenario {:?}",
            baseline.policy.scenario
        ))
    })?;
    let definition =
        Pg7Definition::new(scenario).map_err(|error| CliError::data(error.to_string()))?;
    definition
        .validate_baseline(&baseline)
        .map_err(|error| CliError::data(error.to_string()))?;
    let producer_commit = utf8_argument(producer_commit, "producer commit")?;
    let cache_root_text = utf8_argument(cache_root, "cache root")?;
    let trace_path_text = utf8_argument(trace_path, "trace output path")?;
    let raw_path_text = utf8_argument(raw_path, "raw output path")?;
    if trace_path_text == raw_path_text {
        return Err(CliError::data(
            "trace and raw output paths must be distinct",
        ));
    }
    if cache_root_text != "-"
        && (cache_root_text == trace_path_text || cache_root_text == raw_path_text)
    {
        return Err(CliError::data(
            "cache root, trace output, and raw output paths must be distinct",
        ));
    }
    match scenario {
        Pg7Scenario::FormulaCached if cache_root_text == "-" || cache_root_text.is_empty() => {
            return Err(CliError::data(
                "formula-cached requires a fresh, nonexistent cache-root path",
            ));
        }
        Pg7Scenario::FormulaCold | Pg7Scenario::Text10kGlyph if cache_root_text != "-" => {
            return Err(CliError::data(format!(
                "{scenario} requires '-' for cache-root"
            )));
        }
        _ => {}
    }

    EvidenceRef::from_bytes(EvidenceKind::PhaseTrace, trace_path_text, &[])
        .map_err(|error| CliError::data(error.to_string()))?;
    EvidenceRef::from_bytes(EvidenceKind::RawSamples, raw_path_text, &[])
        .map_err(|error| CliError::data(error.to_string()))?;
    validate_output_parent(trace_path, "trace output")?;
    validate_output_parent(raw_path, "raw output")?;
    refuse_existing(trace_path, "trace output")?;
    refuse_existing(raw_path, "raw output")?;
    require_release_perf_front_door()?;

    let store = if scenario == Pg7Scenario::FormulaCached {
        validate_cache_root(cache_root)?;
        refuse_existing(cache_root, "cache root")?;
        Some(
            Store::open_host(
                Arc::new(StdFs),
                Arc::new(StdClock::new()),
                cache_root_text,
                StoreConfig::default(),
            )
            .map_err(|error| CliError::data(format!("cannot open fresh cache root: {error}")))?,
        )
    } else {
        None
    };
    let artifacts = measure_pg7(&baseline, producer_commit, store.as_ref(), trace_path_text)
        .map_err(|error| CliError::data(error.to_string()))?;
    let raw = artifacts
        .batch
        .to_tsv()
        .map_err(|error| CliError::data(error.to_string()))?;
    let raw_digest = sha256(raw.as_bytes());
    let trace_digest = sha256(artifacts.trace_tsv.as_bytes());
    let valid_samples = artifacts
        .batch
        .samples
        .iter()
        .filter(|sample| sample.invalid_reason.is_none())
        .count();
    let invalid_samples = artifacts.batch.samples.len() - valid_samples;

    write_new(trace_path, artifacts.trace_tsv.as_bytes(), "trace output")?;
    if let Err(error) = write_new(raw_path, raw.as_bytes(), "raw output") {
        return Err(CliError::io(format!(
            "{}; trace output {trace_path_text:?} was already published and was not deleted",
            error.detail
        )));
    }

    Ok(format!(
        "{{\"schema\":\"{CLI_SCHEMA}\",\"kind\":\"pg7-measurement\",\
         \"gate\":\"pg-7\",\"scenario\":\"{}\",\"benchmark_definition\":\"{}\",\
         \"config_digest\":\"{}\",\"producer_commit\":\"{}\",\
         \"sample_count\":{},\"valid_samples\":{},\"invalid_samples\":{},\
         \"bare_metal\":{},\"isolation_qualified\":{},\
         \"result_digest\":\"{}\",\"cache_state\":\"{}\",\
         \"trace_path\":\"{}\",\"trace_digest\":\"{}\",\
         \"raw_path\":\"{}\",\"raw_digest\":\"{}\",\
         \"status\":\"measured-not-evaluated\"}}\n",
        escape_json(&baseline.policy.scenario),
        artifacts.batch.key.benchmark_definition,
        artifacts.batch.key.config_digest,
        producer_commit,
        artifacts.batch.samples.len(),
        valid_samples,
        invalid_samples,
        artifacts.batch.key.bare_metal,
        artifacts.batch.key.isolated,
        artifacts.result_digest,
        artifacts.batch.key.cache_state,
        escape_json(trace_path_text),
        trace_digest,
        escape_json(raw_path_text),
        raw_digest,
    ))
}

fn validate_cache_root(path: &OsStr) -> Result<(), CliError> {
    let path = Path::new(path);
    if path.components().next().is_none()
        || path
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err(CliError::data(
            "cache root is not a canonical relative path",
        ));
    }
    let artifact_root = Path::new("tests/artifacts/perf");
    if !path.starts_with(artifact_root) || path == artifact_root {
        return Err(CliError::data(
            "cache root must be a fresh child below tests/artifacts/perf/",
        ));
    }
    validate_output_parent(path.as_os_str(), "cache root")
}

fn require_release_perf_front_door() -> Result<(), CliError> {
    require_compiled_cargo_profile("release-perf")
        .map_err(|error| CliError::data(error.to_string()))
}

fn utf8_argument<'a>(value: &'a OsStr, label: &str) -> Result<&'a str, CliError> {
    value
        .to_str()
        .ok_or_else(|| CliError::data(format!("{label} is not UTF-8")))
}

fn refuse_existing(path: &OsStr, label: &str) -> Result<(), CliError> {
    match fs::symlink_metadata(path) {
        Ok(_) => Err(CliError::data(format!(
            "{label} already exists; refusing to overwrite it"
        ))),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(CliError::io(format!("cannot inspect {label}: {error}"))),
    }
}

fn validate_output_parent(path: &OsStr, label: &str) -> Result<(), CliError> {
    let parent = Path::new(path)
        .parent()
        .ok_or_else(|| CliError::data(format!("{label} has no parent directory")))?;
    let mut cursor = PathBuf::new();
    for component in parent.components() {
        let Component::Normal(name) = component else {
            return Err(CliError::data(format!(
                "{label} parent is not a canonical relative path"
            )));
        };
        cursor.push(name);
        let metadata = fs::symlink_metadata(&cursor)
            .map_err(|error| CliError::io(format!("cannot inspect {label} parent: {error}")))?;
        if metadata.file_type().is_symlink() {
            return Err(CliError::data(format!(
                "{label} parent contains a symbolic link at {cursor:?}"
            )));
        }
        if !metadata.is_dir() {
            return Err(CliError::data(format!(
                "{label} parent component is not a directory: {cursor:?}"
            )));
        }
    }
    Ok(())
}

fn write_new(path: &OsStr, bytes: &[u8], label: &str) -> Result<(), CliError> {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|error| CliError::io(format!("cannot create {label}: {error}")))?;
    file.write_all(bytes)
        .map_err(|error| CliError::io(format!("cannot write {label}: {error}")))?;
    file.sync_all()
        .map_err(|error| CliError::io(format!("cannot sync {label}: {error}")))
}

fn read_utf8(path: &OsStr, label: &'static str, limit: u64) -> Result<String, CliError> {
    let file = fs::File::open(path)
        .map_err(|error| CliError::data(format!("cannot read {label}: {error}")))?;
    let mut bytes = Vec::new();
    file.take(limit.saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(|error| CliError::data(format!("cannot read {label}: {error}")))?;
    if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > limit {
        return Err(CliError::data(format!(
            "{label} exceeds the {limit}-byte resource limit"
        )));
    }
    String::from_utf8(bytes)
        .map_err(|error| CliError::data(format!("{label} is not UTF-8: {error}")))
}

#[derive(Debug)]
struct CliError {
    exit_code: u8,
    kind: &'static str,
    detail: String,
}

impl CliError {
    fn usage(detail: impl Into<String>) -> Self {
        Self {
            exit_code: EXIT_USAGE,
            kind: "usage",
            detail: detail.into(),
        }
    }

    fn data(detail: impl Into<String>) -> Self {
        Self {
            exit_code: EXIT_DATA,
            kind: "data",
            detail: detail.into(),
        }
    }

    fn io(detail: impl Into<String>) -> Self {
        Self {
            exit_code: EXIT_IO,
            kind: "io",
            detail: detail.into(),
        }
    }

    fn to_ndjson(&self) -> String {
        format!(
            "{{\"schema\":\"{CLI_SCHEMA}\",\"kind\":\"error\",\
             \"error_kind\":\"{}\",\"exit_code\":{},\"detail\":\"{}\"}}",
            self.kind,
            self.exit_code,
            escape_json(&self.detail),
        )
    }
}

fn escape_json(value: &str) -> String {
    let mut output = String::with_capacity(value.len());
    for character in value.chars() {
        match character {
            '"' => output.push_str("\\\""),
            '\\' => output.push_str("\\\\"),
            '\n' => output.push_str("\\n"),
            '\r' => output.push_str("\\r"),
            '\t' => output.push_str("\\t"),
            character if character.is_control() => {
                let _ = write!(output, "\\u{:04x}", character as u32);
            }
            character => output.push(character),
        }
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn usage_errors_are_single_line_robot_records() {
        let error = dispatch(&[]).unwrap_err();
        assert_eq!(error.exit_code, EXIT_USAGE);
        assert_eq!(error.to_ndjson().lines().count(), 1);
        assert!(error.to_ndjson().contains("\"error_kind\":\"usage\""));
    }

    #[test]
    fn robot_error_escaping_cannot_inject_a_record() {
        let error = CliError::data("quote \" and newline\nstay data");
        let record = error.to_ndjson();
        assert_eq!(record.lines().count(), 1);
        assert!(record.contains("quote \\\" and newline\\nstay data"));
    }

    #[test]
    fn release_perf_front_door_is_compile_bound() {
        let result = require_release_perf_front_door();
        let compiled_profile = fmn_conformance::perf::COMPILED_CARGO_PROFILE;
        assert_eq!(result.is_ok(), compiled_profile == "release-perf");
        if let Err(error) = result {
            assert!(error.detail.contains(compiled_profile));
        }
    }

    #[test]
    fn committed_catalog_robot_surface_is_line_oriented_and_complete() {
        let path =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/performance/PERF_GATES.tsv");
        let output = catalog(path.as_os_str()).unwrap();
        assert_eq!(output.lines().count(), 22);
        assert!(output.lines().all(|line| {
            line.starts_with("{\"schema\":\"fmn-perf-cli/1\"") && line.ends_with('}')
        }));
        assert!(output.contains("\"kind\":\"catalog\""));
        assert!(output.contains("\"gate\":\"pg-a\""));
    }

    #[test]
    fn pg2_definition_surface_is_closed_and_line_oriented() {
        let output = pg2_definitions();
        assert_eq!(output.lines().count(), 2);
        assert!(output.lines().all(|line| {
            line.starts_with("{\"schema\":\"fmn-perf-cli/1\"") && line.ends_with('}')
        }));
        assert!(output.contains("\"scenario\":\"fill-canonical\""));
        assert!(output.contains("\"scenario\":\"stroke-canonical\""));
        assert!(!output.contains("\"status\""));
    }

    #[test]
    fn pg7_definition_surface_is_closed_and_line_oriented() {
        let output = pg7_definitions().unwrap();
        assert_eq!(output.lines().count(), 3);
        assert!(output.lines().all(|line| {
            line.starts_with("{\"schema\":\"fmn-perf-cli/1\"") && line.ends_with('}')
        }));
        assert!(output.contains("\"scenario\":\"formula-cold\""));
        assert!(output.contains("\"scenario\":\"formula-cached\""));
        assert!(output.contains("\"scenario\":\"text-10k-glyph\""));
        assert!(!output.contains("\"status\""));
    }

    #[test]
    fn measure_pg2_refuses_ambiguous_argument_counts_before_io() {
        let arguments = vec![std::ffi::OsString::from("measure-pg2")];
        let error = dispatch(&arguments).unwrap_err();
        assert_eq!(error.exit_code, EXIT_USAGE);
        assert!(error.detail.contains("<baseline.tsv>"));
    }

    #[test]
    fn measure_pg7_refuses_ambiguous_argument_counts_before_io() {
        let arguments = vec![std::ffi::OsString::from("measure-pg7")];
        let error = dispatch(&arguments).unwrap_err();
        assert_eq!(error.exit_code, EXIT_USAGE);
        assert!(error.detail.contains("<cache-root-or-dash>"));
    }

    #[test]
    fn output_parent_check_requires_canonical_existing_directories() {
        validate_output_parent(OsStr::new("src/new-output.tsv"), "test output").unwrap();
        let error =
            validate_output_parent(OsStr::new("src/../new-output.tsv"), "test output").unwrap_err();
        assert_eq!(error.kind, "data");
        assert!(error.detail.contains("canonical relative path"));
    }

    #[test]
    fn cache_root_check_is_canonical_and_artifact_scoped() {
        let error = validate_cache_root(OsStr::new("/tmp/pg7-cache")).unwrap_err();
        assert!(error.detail.contains("canonical relative path"));
        let error = validate_cache_root(OsStr::new("src/pg7-cache")).unwrap_err();
        assert!(error.detail.contains("tests/artifacts/perf"));
        let error = validate_cache_root(OsStr::new("tests/artifacts/perf/missing-parent/cache"))
            .unwrap_err();
        assert!(error.detail.contains("cannot inspect cache root parent"));
    }
}
