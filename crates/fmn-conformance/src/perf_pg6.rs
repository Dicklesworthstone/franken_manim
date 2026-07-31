//! Canonical PG-6 primitive steady-state allocation producer.
//!
//! The committed scene-golden corpus is the workload. For every scene, this
//! producer renders one warm frame through a fresh [`FrameArena`], then renders
//! the same scene into the same output buffer through the reused arena. The
//! second frame's engine-owned allocation ledger is one exact policy sample;
//! both frame digests are retained and must match before evidence is emitted.
//!
//! This is intentionally only PG-6's `primitive-steady-allocations` surface.
//! Peak RSS and the one-hour leak soak require different lifecycle authorities
//! and remain separate, explicit evidence gaps.

use crate::perf::{
    Baseline, Direction, Enforcement, EvidenceKind, EvidenceRef, GateId, GateScope,
    MeasurementBatch, MetricUnit, PerfError, Sample, require_compiled_cargo_profile,
    validate_producer_commit,
};
use crate::scene_goldens::{self, SCENES, TILING};
use fmn_hash::{Digest, Sha256, sha256};
use fmn_render::{
    AllocStats, Binning, EngineIdentity, FrameArena, FrameJob, MonoTable, RenderPlan, Tier,
    frame_digest,
};
use std::collections::BTreeSet;
use std::fmt;
use std::fmt::Write as _;

/// Stable workload-definition schema.
pub const PG6_DEFINITION_SCHEMA: &str = "fmn-perf-pg6-definition/1";
/// Stable phase-trace schema.
pub const PG6_TRACE_SCHEMA: &str = "fmn-perf-pg6-trace/1";
/// Policy-catalog scenario implemented by this producer.
pub const PG6_SCENARIO: &str = "primitive-steady-allocations";
/// Policy-catalog minimum valid scene samples.
pub const PG6_MIN_VALID_SAMPLES: usize = 21;
/// An exact invariant admits no invalid scene samples.
pub const PG6_MAX_INVALID_SAMPLES: usize = 0;
/// Every committed scene-golden case contributes one sample.
pub const PG6_SAMPLE_COUNT: usize = 27;
/// The permanent per-commit worker-team size for this producer.
pub const PG6_THREADS: usize = 4;
/// One excluded warm frame sizes the arena and worker pool for each scene.
pub const PG6_WARMUP_FRAMES_PER_SCENE: usize = 1;

const BUILD_PROFILE: &str = "release-perf";
const THREAD_PROFILE: &str = "fixed-4";
const CACHE_STATE: &str = "warm-reused-frame-arena";
const OUTPUT_MODE: &str = "raw-rgba16f";
const SCENE_GOLDEN_LOCK_HEADER: &str = "# fmn-golden-lock v1 suite=scene_goldens key=certified";
const SCENE_GOLDEN_LOCK: &str = include_str!("../goldens/scene_goldens.certified.lock");

// Fixed by the reviewed release-perf corpus proof. The aggregate hashes the
// ordered scene names and both equal frame digests, independently of allocation
// counts, so a rendering/corpus drift cannot silently retain the same producer
// identity.
const EXPECTED_RESULT_DIGEST: Digest = Digest::from_bytes([
    0xd5, 0x45, 0x1d, 0x89, 0xeb, 0x10, 0x50, 0xa6, 0x51, 0x34, 0x03, 0xa0, 0x60, 0x61, 0x75, 0x42,
    0xfa, 0x93, 0x98, 0x1a, 0x67, 0x1b, 0x5f, 0xea, 0x58, 0xb7, 0xbb, 0x86, 0xee, 0xca, 0x6e, 0xaa,
]);

/// The complete content-addressed definition of the allocation workload.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Pg6Definition;

impl Pg6Definition {
    /// Construct the sole canonical PG-6 allocation definition.
    #[must_use]
    pub const fn new() -> Self {
        Self
    }

    /// Exact definition bytes hashed into [`crate::perf::BenchmarkKey`].
    #[must_use]
    pub fn to_tsv(self) -> String {
        let mut output = String::new();
        {
            let mut row = |name: &str, value: &dyn fmt::Display| {
                let _ = writeln!(output, "{name}\t{value}");
            };
            row("schema", &PG6_DEFINITION_SCHEMA);
            row("gate", &GateId::Pg6);
            row("scenario", &PG6_SCENARIO);
            row("unit", &MetricUnit::Allocations.name());
            row("target", &0);
            row("threads", &PG6_THREADS);
            row("warmup_frames_per_scene", &PG6_WARMUP_FRAMES_PER_SCENE);
            row("sample_count", &PG6_SAMPLE_COUNT);
            row("sample_scope", &"one-post-warmup-frame-per-scene");
            row("lifecycle_point", &"post-construct");
            row("frame_index", &0);
            row("engine", &pg6_identity().engine.name());
            row("tier", &pg6_identity().tier.name());
            row("thread_profile", &THREAD_PROFILE);
            row("cache_state", &CACHE_STATE);
            row("output_mode", &OUTPUT_MODE);
            row("frame_width_px", &scene_goldens::WIDTH);
            row("frame_height_px", &scene_goldens::HEIGHT);
            row("scene_golden_lock_digest", &self.corpus_lock_digest());
            row("config_digest", &self.config_digest());
            row("expected_result_digest", &self.expected_result_digest());
        }
        for (index, case) in SCENES.iter().enumerate() {
            let _ = writeln!(output, "scene\t{index}\t{}", case.name);
        }
        output
    }

    /// SHA-256 of [`Self::to_tsv`].
    #[must_use]
    pub fn digest(self) -> Digest {
        sha256(self.to_tsv().as_bytes())
    }

    /// Exact C7/C10 renderer/configuration digest for this artifact.
    #[must_use]
    pub fn config_digest(self) -> Digest {
        fmn_render::engine::journal_digest(pg6_identity(), &scene_goldens::frame_config(), TILING)
    }

    /// Digest of the committed certified corpus lock that fixes scene input.
    #[must_use]
    pub fn corpus_lock_digest(self) -> Digest {
        sha256(SCENE_GOLDEN_LOCK.as_bytes())
    }

    /// Aggregate frame identity required before evidence is emitted.
    #[must_use]
    pub const fn expected_result_digest(self) -> Digest {
        EXPECTED_RESULT_DIGEST
    }

    /// Validate that the embedded lock and compiled corpus name exactly the
    /// same bounded scene set.
    ///
    /// # Errors
    /// Returns a typed fixture error for a malformed, missing, duplicate, or
    /// stale lock row.
    pub fn validate_corpus_lock(self) -> Result<(), Pg6Error> {
        if SCENES.len() != PG6_SAMPLE_COUNT {
            return Err(Pg6Error::Fixture(format!(
                "compiled corpus has {} scenes, expected {PG6_SAMPLE_COUNT}",
                SCENES.len()
            )));
        }
        let mut lines = SCENE_GOLDEN_LOCK.lines();
        if lines.next() != Some(SCENE_GOLDEN_LOCK_HEADER) {
            return Err(Pg6Error::Fixture(
                "scene-golden lock header does not match the certified v1 schema".to_owned(),
            ));
        }
        let expected: BTreeSet<_> = SCENES.iter().map(|case| case.name).collect();
        let mut actual = BTreeSet::new();
        for (index, line) in lines.enumerate() {
            let mut fields = line.split('\t');
            let name = fields.next().unwrap_or_default();
            let length = fields.next().unwrap_or_default();
            let digest = fields.next().unwrap_or_default();
            if name.is_empty()
                || fields.next().is_some()
                || length.parse::<u64>().is_err()
                || Digest::from_hex(digest).is_err()
            {
                return Err(Pg6Error::Fixture(format!(
                    "malformed scene-golden lock row {}",
                    index + 2
                )));
            }
            if !actual.insert(name) {
                return Err(Pg6Error::Fixture(format!(
                    "duplicate scene-golden lock row {name:?}"
                )));
            }
        }
        if actual != expected {
            let missing: Vec<_> = expected.difference(&actual).copied().collect();
            let stale: Vec<_> = actual.difference(&expected).copied().collect();
            return Err(Pg6Error::Fixture(format!(
                "scene-golden lock/corpus mismatch: missing {missing:?}, stale {stale:?}"
            )));
        }
        Ok(())
    }

    /// Validate that a baseline names precisely this producer.
    ///
    /// # Errors
    /// Returns a typed identity error before any corpus scene is built.
    pub fn validate_baseline(self, baseline: &Baseline) -> Result<(), Pg6Error> {
        baseline.validate()?;
        let key = &baseline.key;
        let mut mismatches = Vec::new();
        if baseline.policy.gate != GateId::Pg6 {
            mismatches.push("gate");
        }
        if baseline.policy.scenario != PG6_SCENARIO {
            mismatches.push("scenario");
        }
        if baseline.policy.unit != MetricUnit::Allocations {
            mismatches.push("unit");
        }
        if baseline.policy.direction != Direction::Exactly {
            mismatches.push("direction");
        }
        if baseline.policy.target != Some(0) {
            mismatches.push("target");
        }
        if baseline.policy.min_valid_samples != PG6_MIN_VALID_SAMPLES {
            mismatches.push("min_valid_samples");
        }
        if baseline.policy.max_invalid_samples != PG6_MAX_INVALID_SAMPLES {
            mismatches.push("max_invalid_samples");
        }
        if baseline.policy.max_mad_bps != 0 {
            mismatches.push("max_mad_bps");
        }
        if baseline.policy.alert_regression_bps != 0 {
            mismatches.push("alert_regression_bps");
        }
        if baseline.policy.block_regression_bps != 0 {
            mismatches.push("block_regression_bps");
        }
        if baseline.policy.enforcement != Enforcement::Blocking {
            mismatches.push("enforcement");
        }
        if baseline.policy.scope != GateScope::Core {
            mismatches.push("scope");
        }
        if baseline.policy.require_regression_profile {
            mismatches.push("require_regression_profile");
        }
        if key.benchmark_definition != self.digest() {
            mismatches.push("benchmark_definition");
        }
        if key.config_digest != self.config_digest() {
            mismatches.push("config_digest");
        }
        if key.build_profile != BUILD_PROFILE {
            mismatches.push("build_profile");
        }
        if key.engine != pg6_identity().engine.name() {
            mismatches.push("engine");
        }
        if key.tier != pg6_identity().tier.name() {
            mismatches.push("tier");
        }
        if key.thread_profile != THREAD_PROFILE {
            mismatches.push("thread_profile");
        }
        if key.cache_state != CACHE_STATE {
            mismatches.push("cache_state");
        }
        if key.output_mode != OUTPUT_MODE {
            mismatches.push("output_mode");
        }
        if key.external_tool_fingerprint.is_some() {
            mismatches.push("external_tool_fingerprint");
        }
        if mismatches.is_empty() {
            Ok(())
        } else {
            Err(Pg6Error::Identity(format!(
                "{PG6_SCENARIO} baseline differs from the compiled producer in: {}",
                mismatches.join(", ")
            )))
        }
    }
}

/// One corpus scene's retained warm/reuse proof.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Pg6CaseResult {
    /// Stable scene-golden case name.
    pub scene: &'static str,
    /// Frame identity produced while sizing the arena.
    pub warm_frame_digest: Digest,
    /// Frame identity produced through the reused arena and output buffer.
    pub measured_frame_digest: Digest,
    /// Complete engine-owned ledger for the excluded warm frame.
    pub warm: AllocStats,
    /// Complete engine-owned ledger for the measured frame.
    pub measured: AllocStats,
}

/// Measurement output before the caller persists the two artifacts.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Pg6Artifacts {
    /// Canonical raw measurement bundle, including the trace reference.
    pub batch: MeasurementBatch,
    /// Exact bytes named by the batch's phase-trace evidence row.
    pub trace_tsv: String,
    /// Aggregate identity of every warm/measured frame pair.
    pub result_digest: Digest,
    /// Per-scene allocation and frame-identity proof.
    pub cases: Vec<Pg6CaseResult>,
}

/// Measure every committed corpus scene through one warm and one reused frame.
///
/// `trace_path` is recorded and content-addressed, but this function performs
/// no filesystem I/O. The CLI publishes the returned trace before the raw
/// bundle using exclusive-create semantics.
///
/// # Errors
/// Returns before scene construction for identity, path, profile, or embedded
/// lock errors. Renderer errors and frame drift are explicit. Nonzero measured
/// allocations remain valid samples so the common exact-policy verifier can
/// issue the blocking verdict.
pub fn measure_pg6(
    baseline: &Baseline,
    producer_commit: &str,
    trace_path: impl Into<String>,
) -> Result<Pg6Artifacts, Pg6Error> {
    let definition = Pg6Definition::new();
    definition.validate_baseline(baseline)?;
    validate_producer_commit(producer_commit)?;
    let trace_path = trace_path.into();
    let _ = EvidenceRef::from_bytes(EvidenceKind::PhaseTrace, trace_path.clone(), &[])?;
    require_compiled_cargo_profile(BUILD_PROFILE)?;
    definition.validate_corpus_lock()?;

    let mut batch = MeasurementBatch {
        key: calibration_key(baseline),
        producer_commit: producer_commit.to_owned(),
        samples: Vec::with_capacity(PG6_SAMPLE_COUNT),
        evidence: Vec::new(),
    };
    let _ = batch.to_tsv()?;

    let config = scene_goldens::frame_config();
    let corpus = scene_goldens::corpus();
    let mut cases = Vec::with_capacity(PG6_SAMPLE_COUNT);
    for case in SCENES {
        let built = (case.build)(corpus);
        let mut plan = RenderPlan::new();
        let _ = plan.sync(&built.stage, 0);
        let mono = MonoTable::build(&plan, config.map);
        let mut binning = Binning::build(&plan, config.viewport, TILING, config.map);
        binning
            .prune_occluded(&plan)
            .map_err(|error| Pg6Error::Fixture(error.to_string()))?;

        let mut arena = FrameArena::new();
        let (mut frame, warm_frame_digest, warm) = {
            let job = FrameJob::with_identity_in(
                &mut arena,
                &plan,
                &mono,
                &binning,
                config,
                pg6_identity(),
            )
            .map_err(|error| Pg6Error::Fixture(error.to_string()))?;
            let frame = job
                .render(PG6_THREADS)
                .map_err(|error| Pg6Error::Render(error.to_string()))?;
            let stats = job.allocation_stats();
            let digest =
                frame_digest(&frame).map_err(|error| Pg6Error::Render(error.to_string()))?;
            (frame, digest, stats)
        };

        let (measured_frame_digest, measured) = {
            let job = FrameJob::with_identity_in(
                &mut arena,
                &plan,
                &mono,
                &binning,
                config,
                pg6_identity(),
            )
            .map_err(|error| Pg6Error::Fixture(error.to_string()))?;
            job.render_into(PG6_THREADS, &mut frame)
                .map_err(|error| Pg6Error::Render(error.to_string()))?;
            let stats = job.allocation_stats();
            let digest =
                frame_digest(&frame).map_err(|error| Pg6Error::Render(error.to_string()))?;
            (digest, stats)
        };
        if measured_frame_digest != warm_frame_digest {
            return Err(Pg6Error::Render(format!(
                "{} changed across arena reuse: warm {}, measured {}",
                case.name, warm_frame_digest, measured_frame_digest
            )));
        }
        if warm.heap_allocs_this_frame == 0 {
            return Err(Pg6Error::Render(format!(
                "{} warm frame reported no arena/worker sizing allocations",
                case.name
            )));
        }
        if measured.arena_buffer_bytes != warm.arena_buffer_bytes
            || measured.pool_slots != warm.pool_slots
            || measured.pool_slots != PG6_THREADS
        {
            return Err(Pg6Error::Render(format!(
                "{} storage changed across arena reuse: warm bytes/slots {}/{}, measured {}/{}; expected {PG6_THREADS} worker slots",
                case.name,
                warm.arena_buffer_bytes,
                warm.pool_slots,
                measured.arena_buffer_bytes,
                measured.pool_slots,
            )));
        }
        batch
            .samples
            .push(Sample::valid(measured.heap_allocs_this_frame));
        cases.push(Pg6CaseResult {
            scene: case.name,
            warm_frame_digest,
            measured_frame_digest,
            warm,
            measured,
        });
    }

    let result_digest = aggregate_result_digest(definition.corpus_lock_digest(), &cases)?;
    if result_digest != definition.expected_result_digest() {
        return Err(Pg6Error::Render(format!(
            "corpus result self-golden drift: expected {}, got {}",
            definition.expected_result_digest(),
            result_digest
        )));
    }
    let trace_tsv = render_trace(definition, result_digest, &cases);
    let evidence =
        EvidenceRef::from_bytes(EvidenceKind::PhaseTrace, trace_path, trace_tsv.as_bytes())?;
    batch.evidence.push(evidence);
    let _ = batch.to_tsv()?;

    Ok(Pg6Artifacts {
        batch,
        trace_tsv,
        result_digest,
        cases,
    })
}

/// This build's certified arithmetic at the crate-wide compiled tier.
#[must_use]
pub const fn pg6_identity() -> EngineIdentity {
    EngineIdentity {
        tier: Tier::COMPILED,
        ..EngineIdentity::certified()
    }
}

fn calibration_key(baseline: &Baseline) -> crate::perf::BenchmarkKey {
    let mut key = baseline.key.clone();
    // fm-inr.1 owns live host/profile attestation. Caller-supplied booleans
    // are not evidence, so this producer can calibrate without manufacturing
    // a passing gate on an unattested host.
    key.bare_metal = false;
    key.isolated = false;
    key
}

fn aggregate_result_digest(
    corpus_lock_digest: Digest,
    cases: &[Pg6CaseResult],
) -> Result<Digest, Pg6Error> {
    let mut hash = Sha256::new();
    hash.update(b"fmn-perf-pg6-result-v1");
    hash.update(corpus_lock_digest.as_bytes());
    for case in cases {
        hash_field(&mut hash, case.scene.as_bytes())?;
        hash.update(case.warm_frame_digest.as_bytes());
        hash.update(case.measured_frame_digest.as_bytes());
    }
    Ok(hash.finalize())
}

fn hash_field(hash: &mut Sha256, bytes: &[u8]) -> Result<(), Pg6Error> {
    let length = u64::try_from(bytes.len())
        .map_err(|_| Pg6Error::Fixture("corpus field exceeds u64".to_owned()))?;
    hash.update(&length.to_be_bytes());
    hash.update(bytes);
    Ok(())
}

fn render_trace(
    definition: Pg6Definition,
    result_digest: Digest,
    cases: &[Pg6CaseResult],
) -> String {
    let mut output = String::new();
    {
        let mut row = |name: &str, value: &dyn fmt::Display| {
            let _ = writeln!(output, "{name}\t{value}");
        };
        row("schema", &PG6_TRACE_SCHEMA);
        row("gate", &GateId::Pg6);
        row("scenario", &PG6_SCENARIO);
        row("benchmark_definition", &definition.digest());
        row("config_digest", &definition.config_digest());
        row("scene_golden_lock_digest", &definition.corpus_lock_digest());
        row("engine", &pg6_identity().engine.name());
        row("tier", &pg6_identity().tier.name());
        row("thread_profile", &THREAD_PROFILE);
        row("threads", &PG6_THREADS);
        row("warmup_frames_per_scene", &PG6_WARMUP_FRAMES_PER_SCENE);
        row("sample_count", &cases.len());
        row("result_digest", &result_digest);
    }
    for (index, case) in cases.iter().enumerate() {
        let _ = writeln!(
            output,
            "scene\t{index}\t{}\t{}\t{}\t{}\t{}\t{}\t{}",
            case.scene,
            case.warm_frame_digest,
            case.measured_frame_digest,
            case.warm.heap_allocs_this_frame,
            case.measured.heap_allocs_this_frame,
            case.measured.arena_buffer_bytes,
            case.measured.pool_slots,
        );
    }
    output
}

/// PG-6 producer failure.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Pg6Error {
    /// Common performance schema/evidence failure.
    Perf(PerfError),
    /// Baseline and compiled-producer identity differ.
    Identity(String),
    /// Canonical corpus or derived artifact construction failed.
    Fixture(String),
    /// The real Lumen render or self-golden check failed.
    Render(String),
}

impl fmt::Display for Pg6Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Perf(error) => error.fmt(formatter),
            Self::Identity(detail) => write!(formatter, "PG-6 identity: {detail}"),
            Self::Fixture(detail) => write!(formatter, "PG-6 fixture: {detail}"),
            Self::Render(detail) => write!(formatter, "PG-6 render: {detail}"),
        }
    }
}

impl std::error::Error for Pg6Error {}

impl From<PerfError> for Pg6Error {
    fn from(error: PerfError) -> Self {
        Self::Perf(error)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn definition_binds_the_complete_corpus_and_allocation_axes() {
        let definition = Pg6Definition::new();
        let text = definition.to_tsv();
        assert!(text.contains("scenario\tprimitive-steady-allocations\n"));
        assert!(text.contains("sample_scope\tone-post-warmup-frame-per-scene\n"));
        assert!(text.contains("engine\tcertified-cpu\n"));
        assert!(text.contains(&format!("tier\t{}\n", Tier::COMPILED.name())));
        assert_eq!(
            text.lines()
                .filter(|line| line.starts_with("scene\t"))
                .count(),
            PG6_SAMPLE_COUNT
        );
        if Tier::COMPILED.name() == "portable" {
            assert_eq!(
                definition.digest().to_string(),
                "8fce972337b2e657150e1c8e35485fb6324b0191f9e4ebc71a41c3c3b7faee66"
            );
        }
        assert_eq!(
            definition.expected_result_digest().to_string(),
            "d5451d89eb1050a6513403a060617542fa93981a671b5fea58b7bb86eeca6eaa"
        );
    }

    #[test]
    fn embedded_lock_and_compiled_corpus_are_exactly_aligned() {
        Pg6Definition::new()
            .validate_corpus_lock()
            .expect("committed lock and compiled corpus agree");
    }
}
