use fmn_conformance::perf::{
    Baseline, BenchmarkKey, Direction, Enforcement, GateId, GatePolicy, GateScope,
    MeasurementBatch, MetricUnit, Sample, Verdict,
};
use fmn_conformance::perf_pg6::{
    PG6_MAX_INVALID_SAMPLES, PG6_MIN_VALID_SAMPLES, PG6_SAMPLE_COUNT, PG6_SCENARIO, PG6_THREADS,
    PG6_TRACE_SCHEMA, Pg6Definition, measure_pg6, pg6_identity,
};
use fmn_hash::sha256;

const COMMIT: &str = "0123456789abcdef0123456789abcdef01234567";

fn policy() -> GatePolicy {
    GatePolicy {
        gate: GateId::Pg6,
        scenario: PG6_SCENARIO.to_owned(),
        unit: MetricUnit::Allocations,
        direction: Direction::Exactly,
        target: Some(0),
        min_valid_samples: PG6_MIN_VALID_SAMPLES,
        max_invalid_samples: PG6_MAX_INVALID_SAMPLES,
        max_mad_bps: 0,
        alert_regression_bps: 0,
        block_regression_bps: 0,
        enforcement: Enforcement::Blocking,
        scope: GateScope::Core,
        require_regression_profile: false,
    }
}

fn key() -> BenchmarkKey {
    let definition = Pg6Definition::new();
    BenchmarkKey {
        profile_id: "pg6-test-host".to_owned(),
        build_profile: "release-perf".to_owned(),
        host_fingerprint: sha256(b"host"),
        toolchain_fingerprint: sha256(b"toolchain"),
        suite_lock_digest: sha256(b"suite-lock"),
        benchmark_definition: definition.digest(),
        gate: GateId::Pg6,
        scenario: PG6_SCENARIO.to_owned(),
        unit: MetricUnit::Allocations,
        engine: pg6_identity().engine.name().to_owned(),
        tier: pg6_identity().tier.name().to_owned(),
        thread_profile: "fixed-4".to_owned(),
        execution_plan_digest: sha256(b"execution-plan"),
        config_digest: definition.config_digest(),
        cache_state: "warm-reused-frame-arena".to_owned(),
        output_mode: "raw-rgba16f".to_owned(),
        external_tool_fingerprint: None,
        bare_metal: true,
        isolated: true,
    }
}

fn batch(samples: Vec<Sample>) -> MeasurementBatch {
    MeasurementBatch {
        key: key(),
        producer_commit: COMMIT.to_owned(),
        samples,
        evidence: Vec::new(),
    }
}

#[test]
fn compiled_definition_accepts_only_its_exact_baseline_identity() {
    let definition = Pg6Definition::new();
    let baseline = Baseline::targeted(1, policy(), key(), COMMIT).expect("target baseline");
    definition
        .validate_baseline(&baseline)
        .expect("exact compiled identity");

    let mut wrong = baseline;
    wrong.key.benchmark_definition = sha256(b"other-definition");
    let error = definition
        .validate_baseline(&wrong)
        .expect_err("definition drift must fail before corpus construction");
    assert!(error.to_string().contains("benchmark_definition"));
}

#[test]
fn producer_refuses_bad_commit_before_profile_or_corpus_setup() {
    let baseline = Baseline::targeted(1, policy(), key(), COMMIT).expect("target baseline");
    let error = measure_pg6(
        &baseline,
        "not-a-commit",
        "tests/artifacts/perf/pg6-preflight/trace.tsv",
    )
    .expect_err("producer commit must fail before profile or corpus setup");
    assert!(error.to_string().contains("producer_commit"), "{error}");
}

#[test]
fn producer_refuses_bad_trace_path_before_profile_or_corpus_setup() {
    let baseline = Baseline::targeted(1, policy(), key(), COMMIT).expect("target baseline");
    let error = measure_pg6(&baseline, COMMIT, "outside.tsv")
        .expect_err("trace path must fail before profile or corpus setup");
    assert!(error.to_string().contains("artifact path"), "{error}");
}

#[test]
fn one_injected_allocation_blocks_through_the_common_verifier() {
    let baseline_batch = batch(vec![Sample::valid(0); PG6_SAMPLE_COUNT]);
    let baseline = Baseline::observed(
        1,
        policy(),
        &baseline_batch,
        "tests/artifacts/perf/pg6-steady-allocations/baseline.tsv",
    )
    .expect("observed exact-zero baseline");
    let mut samples = vec![Sample::valid(0); PG6_SAMPLE_COUNT];
    samples[PG6_SAMPLE_COUNT - 1] = Sample::valid(1);
    let report = baseline.evaluate(Some(&baseline_batch), &batch(samples));
    assert_eq!(report.verdict, Verdict::Block);
    assert!(
        report
            .findings
            .iter()
            .any(|finding| finding.code == "target-miss")
    );
}

#[test]
#[ignore = "real four-thread corpus render; run explicitly under --profile release-perf"]
fn release_perf_producer_emits_every_zero_allocation_scene_and_replayable_trace() {
    let definition = Pg6Definition::new();
    let baseline = Baseline::targeted(1, policy(), key(), COMMIT).expect("target baseline");
    let artifacts = measure_pg6(
        &baseline,
        COMMIT,
        "tests/artifacts/perf/pg6-steady-allocations/trace.tsv",
    )
    .expect("release-perf corpus producer");

    assert_eq!(artifacts.cases.len(), PG6_SAMPLE_COUNT);
    assert_eq!(artifacts.batch.samples.len(), PG6_SAMPLE_COUNT);
    assert!(
        artifacts
            .batch
            .samples
            .iter()
            .all(|sample| sample.invalid_reason.is_none() && sample.value == 0)
    );
    assert!(artifacts.cases.iter().all(|case| {
        case.warm_frame_digest == case.measured_frame_digest
            && case.warm.heap_allocs_this_frame > 0
            && case.measured.heap_allocs_this_frame == 0
            && case.warm.arena_buffer_bytes == case.measured.arena_buffer_bytes
            && case.warm.pool_slots == case.measured.pool_slots
            && case.measured.pool_slots == PG6_THREADS
    }));
    assert_eq!(artifacts.result_digest, definition.expected_result_digest());
    assert!(
        artifacts
            .trace_tsv
            .starts_with(&format!("schema\t{PG6_TRACE_SCHEMA}\n"))
    );
    assert_eq!(
        artifacts
            .trace_tsv
            .lines()
            .filter(|line| line.starts_with("scene\t"))
            .count(),
        PG6_SAMPLE_COUNT
    );
    assert!(!artifacts.batch.key.bare_metal);
    assert!(!artifacts.batch.key.isolated);
    let raw = artifacts.batch.to_tsv().expect("canonical raw bundle");
    assert_eq!(
        MeasurementBatch::from_tsv(&raw).expect("raw bundle replay"),
        artifacts.batch
    );
}
