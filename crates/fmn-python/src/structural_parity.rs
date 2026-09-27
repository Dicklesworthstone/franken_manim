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

fn check(
    suite: &'static str,
    reference: &'static str,
    sweep: Option<&'static str>,
) -> Result<StructuralParityReport, String> {
    crate::with_python_test_module(suite, |py, _module, globals| {
        let source = std::ffi::CString::new(include_str!(
            "../../fmn-conformance/python/structural_facts.py"
        ))
        .map_err(|error| error.to_string())?;
        py.run(source.as_c_str(), Some(globals), Some(globals))
            .inspect_err(|error| error.print(py))
            .map_err(|error| error.to_string())?;
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
        assert_eq!(report.compared, 25, "{}", report.summary);
        assert_eq!(
            report.equal + report.equal_with_exclusions,
            25,
            "{}",
            report.summary
        );
    }

    #[test]
    fn embedded_portal_matches_the_reference_class_sweep() {
        let report = super::run_portal_gauntlet_class_sweep().unwrap();
        assert_eq!(report.compared, 284, "{}", report.summary);
        assert_eq!(
            report.equal + report.equal_with_exclusions,
            284,
            "{}",
            report.summary
        );
    }
}
