//! Structural parity with the pinned Reference (fm-5wq.36, fm-5wq.26).
//!
//! The same `structural_facts.py` that the gate runs against the installed
//! wheel runs here inside the embedded portal. It compares the fixed
//! construction set, or the Appendix-A class sweep, with the checked-in
//! Reference facts under the BN-keyed exclusion table.

use pyo3::prelude::*;
use pyo3::types::PyDictMethods;

const EXCLUSIONS: &str =
    include_str!("../../fmn-conformance/fixtures/structural_facts/exclusions.json");

/// What one embedded comparison observed.
#[derive(Debug)]
pub struct StructuralParityReport {
    /// Subjects present on both sides and compared.
    pub compared: u64,
    /// Subjects equal with no exclusion applied.
    pub equal: u64,
    /// Subjects equal only through Behavior-Note, ADR or open-bead rows.
    pub equal_with_exclusions: u64,
    /// The canonical summary line, for logs.
    pub summary: String,
}

/// Extract the fixed construction set in the embedded portal and diff it
/// against the Reference facts. Unexcluded differences, one-sided subjects,
/// stale open-bead rows and an empty comparison are errors.
///
/// # Errors
/// The failing subjects or stale rows, as the canonical summary line.
pub fn run_portal_gauntlet_structural_facts() -> Result<StructuralParityReport, String> {
    check(
        "structural facts",
        include_str!(
            "../../fmn-conformance/fixtures/structural_facts/reference_constructions.v1.ndjson"
        ),
        None,
    )
}

/// The Appendix-A class sweep (every public mobject class, default and
/// declared calls, public methods included) in the embedded portal, against
/// the Reference's facts. Same error rules as the construction check.
///
/// # Errors
/// The failing subjects or stale rows, as the canonical summary line.
pub fn run_portal_gauntlet_class_sweep() -> Result<StructuralParityReport, String> {
    check(
        "structural class sweep",
        include_str!("../../fmn-conformance/fixtures/structural_facts/reference_classes.v1.ndjson"),
        Some(include_str!(
            "../../fmn-conformance/fixtures/structural_facts/class_sweep.json"
        )),
    )
}

/// One minimized corpus construct checked against the pinned Reference.
#[derive(Debug)]
pub struct CorpusRegressionCase {
    /// The bead that fixed it.
    pub bead: String,
    /// The construct, as source.
    pub construct: String,
    /// What the embedded portal shows.
    pub observed: String,
    /// What the pinned Reference showed.
    pub expected: String,
    /// Where `expected` comes from (Reference file and function, measured).
    pub provenance: String,
}

/// Minimized constructs from corpus scenes the portal once refused or
/// rendered differently, and Reference helpers it once computed differently
/// (`tests/reference_corpus_regressions.py`), each checked against what the
/// pinned Reference showed.
///
/// # Errors
/// A case whose observation differs from its expectation, or no cases.
pub fn run_portal_gauntlet_reference_corpus_regressions()
-> Result<Vec<CorpusRegressionCase>, String> {
    crate::with_python_test_module("reference corpus regressions", |py, _module, globals| {
        let code = std::ffi::CString::new(include_str!("../tests/reference_corpus_regressions.py"))
            .map_err(|error| error.to_string())?;
        py.run(code.as_c_str(), Some(globals), Some(globals))
            .inspect_err(|error| error.print(py))
            .map_err(|error| error.to_string())?;
        let records = globals
            .get_item("run_reference_corpus_regressions")
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "corpus regressions entry point is absent".to_owned())?
            .call0()
            .inspect_err(|error| error.print(py))
            .map_err(|error| error.to_string())?;
        let mut cases = Vec::new();
        for record in records.try_iter().map_err(|error| error.to_string())? {
            let record = record.map_err(|error| error.to_string())?;
            let text = |key: &str| -> Result<String, String> {
                record
                    .get_item(key)
                    .and_then(|value| value.extract())
                    .map_err(|error| format!("{key}: {error}"))
            };
            let case = CorpusRegressionCase {
                bead: text("bead")?,
                construct: text("construct")?,
                observed: text("observed")?,
                expected: text("expected")?,
                provenance: text("provenance")?,
            };
            if case.observed != case.expected {
                return Err(format!(
                    "{} {}: observed {:?}, the Reference showed {:?}",
                    case.bead, case.construct, case.observed, case.expected
                ));
            }
            cases.push(case);
        }
        if cases.is_empty() {
            return Err("no corpus regression cases ran".to_owned());
        }
        Ok(cases)
    })
}

/// What the Parity Ledger's witness-registry drill observed (fm-5wq.12).
#[derive(Debug)]
pub struct WitnessRegistryReport {
    /// Unit cases run from `scripts/test_portal_witnesses.py`.
    pub cases: u64,
    /// Cases that failed or raised; zero on a pass.
    pub failed: u64,
}

/// Run the witness registry's unit suite in the embedded portal.
///
/// The suite drives the registry over synthetic modules and plants one defect per failure class
/// the gate's check must catch: a lost witness, a suite that ran no case, a placeholder or
/// unresolved row, and trivial or shifted mutants. The full check, over every gate suite and the
/// installed wheel, stays the last step of `scripts/check_portal_runtime.sh`.
///
/// # Errors
/// The suite cannot load or its summary cannot be read. Failing cases are counted in the
/// report, so the scenario logs them before it fails.
pub fn run_portal_gauntlet_witness_registry() -> Result<WitnessRegistryReport, String> {
    crate::with_python_test_module("witness registry", |py, _module, globals| {
        let scripts = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../scripts");
        let scripts = scripts
            .canonicalize()
            .map_err(|error| format!("scripts directory: {error}"))?;
        globals
            .set_item("_fmn_scripts", scripts.to_string_lossy().as_ref())
            .map_err(|error| error.to_string())?;
        py.run(
            c"import importlib, sys, unittest\n\
              sys.path.insert(0, _fmn_scripts)\n\
              try:\n\
              \x20   _tests = importlib.import_module('test_portal_witnesses')\n\
              \x20   _result = unittest.TextTestRunner(verbosity=1).run(\n\
              \x20       unittest.defaultTestLoader.loadTestsFromModule(_tests))\n\
              finally:\n\
              \x20   sys.path.remove(_fmn_scripts)\n\
              \x20   for _name in ('test_portal_witnesses', 'portal_witnesses'):\n\
              \x20       sys.modules.pop(_name, None)\n\
              _summary = (_result.testsRun, len(_result.failures) + len(_result.errors))\n",
            Some(globals),
            Some(globals),
        )
        .inspect_err(|error| error.print(py))
        .map_err(|error| error.to_string())?;
        let (cases, failed) = globals
            .get_item("_summary")
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "witness registry summary is absent".to_owned())?
            .extract::<(u64, u64)>()
            .map_err(|error| error.to_string())?;
        Ok(WitnessRegistryReport { cases, failed })
    })
}

/// What the Tex selection parity check observed (fm-portal-span-parity-sc4k).
#[derive(Debug)]
pub struct TexSelectionReport {
    /// Rows compared: selections, path counts and matching pairings.
    pub rows: u64,
    /// Rows where the portal gives the Reference's answer.
    pub matches: u64,
    /// Rows where the Reference contradicts its own label-based answer and
    /// the portal gives that answer (Appendix C-36), or where the
    /// Reference's labelling braces make LaTeX refuse the formula (C-37).
    pub reference_defects: u64,
    /// Matching pairings that differ only by the Reference's leftover shape
    /// matching, which the portal deliberately drops (BN-05).
    pub behavior_notes: u64,
    /// Mismatches an open bead owns.
    pub owned: u64,
    /// Mismatches raised by a planted portal whose `getitem` answer is one
    /// glyph off; the check fails when they are too few to notice it.
    pub planted_mismatches: u64,
}

/// `tests/tex_selection_reference.py` in the embedded portal, against the
/// pinned Reference's selections
/// (`fixtures/tex_selection_reference.v1.ndjson`): `tex[sel]`, `t2c`,
/// `isolate`, `substr_to_path_count` and `TransformMatchingTex` pairings.
///
/// # Errors
/// Rows that mismatch with no owner or recorded cause, a stale owned row,
/// fewer than 95% of rows agreeing, or a planted one-glyph shift that the
/// comparison does not notice.
pub fn run_portal_gauntlet_tex_selection() -> Result<TexSelectionReport, String> {
    crate::with_python_test_module("tex selection reference", |py, _module, globals| {
        let code = std::ffi::CString::new(include_str!("../tests/tex_selection_reference.py"))
            .map_err(|error| error.to_string())?;
        py.run(code.as_c_str(), Some(globals), Some(globals))
            .inspect_err(|error| error.print(py))
            .map_err(|error| error.to_string())?;
        let summary = globals
            .get_item("run_tex_selection_reference")
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "tex selection entry point is absent".to_owned())?
            .call1((include_str!(
                "../../fmn-conformance/fixtures/tex_selection_reference.v1.ndjson"
            ),))
            .inspect_err(|error| error.print(py))
            .map_err(|error| error.to_string())?;
        let count = |key: &str| -> Result<u64, String> {
            summary
                .get_item(key)
                .and_then(|value| value.extract::<u64>())
                .map_err(|error| format!("{key}: {error}"))
        };
        Ok(TexSelectionReport {
            rows: count("rows")?,
            matches: count("match")?,
            reference_defects: count("reference_defect")?,
            behavior_notes: count("behavior_note")?,
            owned: count("owned")?,
            planted_mismatches: count("planted_mismatches")?,
        })
    })
}

/// What the envelope drill observed.
#[derive(Debug)]
pub struct EnvelopeDrillReport {
    /// The construction the drill extracted and then planted a defect into.
    pub subject: String,
    /// Its verdict as extracted: within the table's envelopes.
    pub clean_verdict: String,
    /// Its verdict with the root's height doubled: a difference.
    pub planted_verdict: String,
    /// The exclusion row whose envelope the planted defect broke.
    pub violated_row: String,
    /// The planted extent change, relative to the Reference's extent.
    pub size_rel: f64,
}

/// One envelope violation detected end to end (fm-5wq.45): the embedded
/// portal's `Tex` construction passes the exclusion table, and the same facts
/// with the root's height doubled fail it, naming the Behavior-Note envelope
/// that bounds `Tex` size. The blanket bbox rows this replaced admitted both.
///
/// # Errors
/// A clean subject that already differs, a planted one that does not, or a
/// difference reported without an envelope violation.
pub fn run_portal_gauntlet_envelope_drill() -> Result<EnvelopeDrillReport, String> {
    crate::with_python_test_module("structural envelope drill", |py, _module, globals| {
        load_structural_facts(py, globals)?;
        let report = globals
            .get_item("envelope_drill")
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "envelope drill entry point is absent".to_owned())?
            .call1((
                include_str!(
                    "../../fmn-conformance/fixtures/structural_facts/reference_constructions.v1.ndjson"
                ),
                EXCLUSIONS,
                "embedded-portal",
            ))
            .inspect_err(|error| error.print(py))
            .map_err(|error| error.to_string())?;
        let text = |key: &str| -> Result<String, String> {
            report
                .get_item(key)
                .and_then(|value| value.extract())
                .map_err(|error| format!("{key}: {error}"))
        };
        let (subject, clean_verdict, planted_verdict) = (
            text("subject")?,
            text("clean_verdict")?,
            text("planted_verdict")?,
        );
        if clean_verdict == "differs" || planted_verdict != "differs" {
            return Err(format!(
                "{subject}: clean {clean_verdict}, planted {planted_verdict}"
            ));
        }
        let violation = report
            .get_item("violation")
            .map_err(|error| error.to_string())?;
        if violation.is_none() {
            return Err(format!(
                "{subject}: the planted difference names no envelope"
            ));
        }
        Ok(EnvelopeDrillReport {
            subject,
            clean_verdict,
            planted_verdict,
            violated_row: violation
                .get_item("row")
                .and_then(|value| value.extract())
                .map_err(|error| error.to_string())?,
            size_rel: violation
                .get_item("size_rel")
                .and_then(|value| value.extract())
                .map_err(|error| error.to_string())?,
        })
    })
}

fn load_structural_facts(
    py: Python<'_>,
    globals: &Bound<'_, pyo3::types::PyDict>,
) -> Result<(), String> {
    let source = std::ffi::CString::new(include_str!(
        "../../fmn-conformance/python/structural_facts.py"
    ))
    .map_err(|error| error.to_string())?;
    py.run(source.as_c_str(), Some(globals), Some(globals))
        .inspect_err(|error| error.print(py))
        .map_err(|error| error.to_string())
}

fn check(
    suite: &'static str,
    reference: &'static str,
    sweep: Option<&'static str>,
) -> Result<StructuralParityReport, String> {
    crate::with_python_test_module(suite, |py, _module, globals| {
        load_structural_facts(py, globals)?;
        let summary = globals
            .get_item("check_constructions")
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "structural facts entry point is absent".to_owned())?
            .call1((reference, EXCLUSIONS, "embedded-portal", sweep))
            .inspect_err(|error| error.print(py))
            .map_err(|error| error.to_string())?;
        let canonical: String = globals
            .get_item("canonical")
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "structural facts encoder is absent".to_owned())?
            .call1((&summary,))
            .and_then(|value| value.extract())
            .map_err(|error| error.to_string())?;
        let failing = summary
            .get_item("failing")
            .and_then(|value| value.len())
            .map_err(|error| error.to_string())?;
        let stale = summary
            .get_item("stale_open_bead_exclusions")
            .and_then(|value| value.len())
            .map_err(|error| error.to_string())?;
        if failing != 0 || stale != 0 {
            return Err(canonical);
        }
        let count = |key: &str| -> Result<u64, String> {
            let counts = summary
                .get_item("summary")
                .map_err(|error| error.to_string())?;
            match counts.get_item(key) {
                Ok(value) => value
                    .extract::<u64>()
                    .map_err(|error: PyErr| error.to_string()),
                // A verdict with no subjects is absent from the summary.
                Err(_) => Ok(0),
            }
        };
        Ok(StructuralParityReport {
            compared: summary
                .get_item("compared")
                .and_then(|value| value.extract())
                .map_err(|error| error.to_string())?,
            equal: count("equal")?,
            equal_with_exclusions: count("equal-with-exclusions")?,
            summary: canonical,
        })
    })
}

#[cfg(test)]
mod tests {
    #[test]
    fn embedded_portal_matches_the_reference_structure() {
        let report = super::run_portal_gauntlet_structural_facts().unwrap();
        assert_eq!(report.compared, 26, "{}", report.summary);
        assert_eq!(
            report.equal + report.equal_with_exclusions,
            26,
            "{}",
            report.summary
        );
    }

    #[test]
    fn a_planted_tall_tex_breaks_its_envelope_in_the_embedded_portal() {
        let report = super::run_portal_gauntlet_envelope_drill().unwrap();
        assert_eq!(report.subject, "tex", "{report:?}");
        assert_ne!(report.clean_verdict, "differs", "{report:?}");
        assert_eq!(report.planted_verdict, "differs", "{report:?}");
        assert_eq!(report.violated_row, "bn05-text-geometry", "{report:?}");
        assert!(report.size_rel > 0.9, "{report:?}");
    }

    #[test]
    fn witness_registry_drill_catches_every_planted_failure_class() {
        let report = super::run_portal_gauntlet_witness_registry().unwrap();
        assert!(report.cases >= 15, "{report:?}");
        assert_eq!(report.failed, 0, "{report:?}");
    }

    #[test]
    fn embedded_portal_matches_the_reference_corpus_regressions() {
        let cases = super::run_portal_gauntlet_reference_corpus_regressions().unwrap();
        assert_eq!(cases.len(), 17, "{cases:?}");
        assert_eq!(
            cases.iter().filter(|case| case.bead == "fm-5wq.11").count(),
            9,
            "{cases:?}"
        );
        assert_eq!(
            cases.iter().filter(|case| case.bead == "fm-k00v").count(),
            4,
            "{cases:?}"
        );
        for case in &cases {
            assert!(case.bead.starts_with("fm-"), "{case:?}");
            assert!(case.provenance.contains("manimlib/"), "{case:?}");
        }
    }

    #[test]
    fn embedded_portal_matches_the_reference_class_sweep() {
        let report = super::run_portal_gauntlet_class_sweep().unwrap();
        assert_eq!(report.compared, 289, "{}", report.summary);
        assert_eq!(
            report.equal + report.equal_with_exclusions,
            289,
            "{}",
            report.summary
        );
    }
}
