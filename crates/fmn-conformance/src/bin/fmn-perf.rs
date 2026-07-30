//! Dependency-free robot front door for performance policy and evidence.

#![forbid(unsafe_code)]

use fmn_conformance::perf::{
    BASELINE_SCHEMA, Baseline, MeasurementBatch, POLICY_SCHEMA, SAMPLES_SCHEMA,
    parse_policy_catalog, render_policy_catalog,
};
use fmn_hash::sha256;
use std::ffi::OsStr;
use std::fmt::Write as _;
use std::fs;
use std::io::{Read as _, Write as _};
use std::process::ExitCode;

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
            "expected `catalog <PERF_GATES.tsv>` or `verify-baseline <baseline.tsv>`",
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
        "catalog" | "verify-baseline" => Err(CliError::usage(format!(
            "{command} requires exactly one path argument"
        ))),
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
    fn committed_catalog_robot_surface_is_line_oriented_and_complete() {
        let path =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/performance/PERF_GATES.tsv");
        let output = catalog(path.as_os_str()).unwrap();
        assert_eq!(output.lines().count(), 19);
        assert!(output.lines().all(|line| {
            line.starts_with("{\"schema\":\"fmn-perf-cli/1\"") && line.ends_with('}')
        }));
        assert!(output.contains("\"kind\":\"catalog\""));
        assert!(output.contains("\"gate\":\"pg-a\""));
    }
}
