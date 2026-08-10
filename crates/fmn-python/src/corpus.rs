//! The G4a corpus-import harness — fm-d3gt (fm-rqc's structural half).
//!
//! Drives every allowlisted `VIDEO_CORPUS.lock` seed module through the
//! production manimlib bridge with the pinned `3b1b/videos` tree on
//! `sys.path` — the import-virtualization shim of §15.3: the era shim
//! `manim_imports_ext` and its `custom/**` closure resolve inside the
//! pinned tree while `manimlib` itself is served by this crate.
//!
//! The contract this first slice enforces is the corpus gate's error
//! doctrine, not scene success: every seed module must produce a
//! PRECISE, DETERMINISTIC outcome — it imports and its locked scene
//! class is present, or it fails with a named Python exception — never
//! a hang, never a garbled state. The per-module report this test
//! prints is the parity worklist that drives the bridge toward G4a;
//! structural assertions (object counts, timings, bounding envelopes)
//! land on top of the modules that import.
//!
//! Environment: needs host CPython + NumPy (like every test in this
//! crate) and the gitignored `scripts/videos_ref` checkout (the G0-4
//! convention); without the checkout the test skips loudly.

use std::path::{Path, PathBuf};

use pyo3::prelude::*;
use pyo3::types::{PyModule, PyString};

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("the workspace root exists")
}

/// The allowlisted (scene, module) pairs from `VIDEO_CORPUS.lock`.
fn locked_seed() -> Vec<(String, String)> {
    let lock = std::fs::read_to_string(repo_root().join("VIDEO_CORPUS.lock"))
        .expect("VIDEO_CORPUS.lock is committed");
    let mut rows = Vec::new();
    let mut in_scenes = false;
    for line in lock.lines() {
        let trimmed = line.trim_end();
        if trimmed.starts_with('[') {
            in_scenes = trimmed == "[scenes]";
            continue;
        }
        if !in_scenes || trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        let fields: Vec<&str> = trimmed.split('\t').collect();
        assert_eq!(fields.len(), 7, "lock scene row shape: {trimmed}");
        if fields[4] == "allowlisted" {
            rows.push((fields[0].to_owned(), fields[1].to_owned()));
        }
    }
    assert!(!rows.is_empty(), "the seed allowlist is non-empty");
    rows
}

/// One module's harness verdict.
enum Outcome {
    /// Imported; the locked scene class is present in the module.
    Imported,
    /// A named Python exception, rendered `Type: message`.
    Refused(String),
}

/// Phase 2: instantiate the locked scene class and drive the manim
/// lifecycle (`setup -> construct -> tear_down`). The frontier report —
/// which named symbol each scene first misses — is fm-d3gt's structural
/// worklist; a completed scene returns its instance so the harness can
/// take the structural facts the baseline table locks.
fn run_outcome<'py>(
    py: Python<'py>,
    module_path: &str,
    scene: &str,
) -> (Outcome, Option<Bound<'py, PyAny>>) {
    let module_name = module_path.trim_end_matches(".py").replace('/', ".");
    let importlib = py.import("importlib").expect("importlib");
    let module = importlib
        .call_method1("import_module", (module_name.as_str(),))
        .expect("phase 2 runs only after a clean import");
    let class = module.getattr(scene).expect("locked scene class present");
    let outcome = class.call0().and_then(|instance| {
        instance.call_method0("run")?;
        Ok(instance)
    });
    match outcome {
        Ok(instance) => (Outcome::Imported, Some(instance)),
        Err(error) => (Outcome::Refused(render_refusal(py, &error)), None),
    }
}

/// One completed scene's structural facts, formatted at a stable 1e-6
/// tolerance: root count, family total, scene bbox (zero boxes skipped),
/// rational-clock duration, and the final camera (height, theta, phi,
/// center). This line IS the committed baseline row's payload.
fn scene_facts(py: Python<'_>, instance: &Bound<'_, PyAny>) -> PyResult<String> {
    let code = std::ffi::CString::new(
        r#"
import numpy as _np
_roots = scene.mobjects
_family = sum(m.family_size() for m in _roots)
_boxes = [_np.array(m.get_bounding_box(), dtype=float) for m in _roots]
_boxes = [b for b in _boxes if _np.any(b != 0)]
if _boxes:
    _lo = _np.min([b[0] for b in _boxes], axis=0)
    _hi = _np.max([b[2] for b in _boxes], axis=0)
else:
    _lo = _np.zeros(3)
    _hi = _np.zeros(3)
_frame = scene.frame
_center = _frame.get_center()
_values = [
    *_lo, *_hi, scene.time(),
    _frame.get_height(), _frame.get_theta(), _frame.get_phi(), *_center,
]
facts = "\t".join(
    [str(len(_roots)), str(int(_family))]
    + ["%.6f" % float(v) for v in _values]
)
"#,
    )
    .expect("facts snippet contains no NUL");
    let globals = pyo3::types::PyDict::new(py);
    globals.set_item("scene", instance)?;
    py.run(code.as_c_str(), Some(&globals), Some(&globals))?;
    globals
        .get_item("facts")?
        .expect("snippet defines facts")
        .extract()
}

/// The committed per-scene baseline table (fm-rqc's acceptance shape):
/// `scene<TAB>facts...` rows beside the harness, TSV like the corpus
/// lock itself.
fn baseline_path() -> PathBuf {
    repo_root().join("crates/fmn-python/tests/corpus_baselines.tsv")
}

fn read_baselines() -> std::collections::HashMap<String, String> {
    let mut out = std::collections::HashMap::new();
    let Ok(text) = std::fs::read_to_string(baseline_path()) else {
        return out;
    };
    for line in text.lines() {
        let trimmed = line.trim_end();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        if let Some((scene, facts)) = trimmed.split_once('\t') {
            out.insert(scene.to_owned(), facts.to_owned());
        }
    }
    out
}

fn write_baselines(rows: &std::collections::BTreeMap<String, String>) {
    let mut text = String::from(
        "# Structural baselines for corpus scenes that complete end-to-end\n\
         # (fm-d3gt / fm-rqc). Columns after the scene name:\n\
         # roots, family, bbox lo x/y/z, bbox hi x/y/z, duration,\n\
         # camera height, theta, phi, center x/y/z — all at 1e-6.\n\
         # Bless ritual: FMN_CORPUS_BLESS=1 cargo test -p fmn-python corpus\n",
    );
    for (scene, facts) in rows {
        text.push_str(scene);
        text.push('\t');
        text.push_str(facts);
        text.push('\n');
    }
    std::fs::write(baseline_path(), text).expect("write corpus baselines");
}

fn render_refusal(py: Python<'_>, error: &PyErr) -> String {
    let type_name = error.get_type(py).name().map_or_else(
        |_| "UnknownExceptionType".to_owned(),
        |name| name.to_string(),
    );
    let deepest = error
        .traceback(py)
        .and_then(|tb| tb.format().ok())
        .and_then(|text| {
            text.lines()
                .rev()
                .find(|line| line.trim_start().starts_with("File "))
                .map(|line| line.trim().to_owned())
        })
        .unwrap_or_default();
    format!("{type_name}: {} [{deepest}]", error.value(py))
}

fn import_outcome(py: Python<'_>, module_path: &str, scene: &str) -> Outcome {
    let module_name = module_path.trim_end_matches(".py").replace('/', ".");
    let importlib = py.import("importlib").expect("importlib");
    match importlib.call_method1("import_module", (module_name.as_str(),)) {
        Ok(module) => {
            if module.hasattr(scene).unwrap_or(false) {
                Outcome::Imported
            } else {
                Outcome::Refused(format!(
                    "ImportedButSceneMissing: {scene} not defined by {module_path}"
                ))
            }
        }
        Err(error) => Outcome::Refused(render_refusal(py, &error)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every allowlisted seed module reaches a precise, deterministic
    /// verdict through the bridge — the corpus gate's error doctrine.
    /// The printed report is fm-d3gt's live parity worklist.
    #[test]
    fn seed_modules_reach_precise_verdicts_under_the_import_shim() {
        let videos_ref = repo_root().join("scripts/videos_ref");
        if !videos_ref.join(".git").exists() {
            eprintln!(
                "SKIP: scripts/videos_ref checkout absent (G0-4 convention); \
                 run scripts/video_corpus.py verify for the clone commands"
            );
            return;
        }

        let _lock = crate::python_test_lock();
        Python::initialize();
        Python::attach(|py| {
            let module = PyModule::new(py, "manimlib").expect("create bridge module");
            py.import("sys")
                .expect("sys")
                .getattr("modules")
                .expect("sys.modules")
                .set_item("manimlib", &module)
                .expect("install manimlib");
            crate::manimlib(py, &module).expect("initialize manimlib");
            put_videos_ref_on_sys_path(py, &videos_ref);

            let mut refused = 0_usize;
            for (scene, module_path) in locked_seed() {
                match import_outcome(py, &module_path, &scene) {
                    Outcome::Imported => {
                        println!("corpus-import ok       {scene} ({module_path})");
                    }
                    Outcome::Refused(reason) => {
                        refused += 1;
                        println!("corpus-import refused  {scene} ({module_path}): {reason}");
                        // The doctrine: refusals are precise named errors,
                        // never silence or garble.
                        assert!(
                            !reason.trim().is_empty()
                                && !reason.starts_with("UnknownExceptionType"),
                            "refusal must be a named error: {reason}"
                        );
                    }
                }
            }
            println!(
                "corpus-import summary: {} allowlisted, {refused} refused",
                locked_seed().len()
            );
            // The seed allowlist's import contract is now green and
            // stays green: an allowlisted scene that stops importing is
            // a bridge regression, not a worklist entry.
            assert_eq!(refused, 0, "allowlisted seed modules must import");

            // Phase 2, the structural frontier: instantiate each scene
            // and drive the manim lifecycle. Refusals are the parity
            // worklist (reported, precise, unasserted — the frontier
            // moves as the mobject/animation surface lands); a COMPLETED
            // scene must match its committed structural baseline.
            let baselines = read_baselines();
            let bless = std::env::var("FMN_CORPUS_BLESS").is_ok_and(|value| value == "1");
            let mut completed: Vec<(String, String)> = Vec::new();
            for (scene, module_path) in locked_seed() {
                match run_outcome(py, &module_path, &scene) {
                    (Outcome::Imported, instance) => {
                        let instance = instance.expect("completed run returns its instance");
                        let facts = scene_facts(py, &instance)
                            .expect("structural facts of a completed scene");
                        println!("corpus-run    ok       {scene} ({module_path})");
                        println!("corpus-facts  {scene}\t{facts}");
                        completed.push((scene, facts));
                    }
                    (Outcome::Refused(reason), _) => {
                        println!("corpus-run    frontier {scene} ({module_path}): {reason}");
                        assert!(
                            !reason.trim().is_empty()
                                && !reason.starts_with("UnknownExceptionType"),
                            "frontier must be a named error: {reason}"
                        );
                    }
                }
            }

            // The baseline lock. A completed scene without a blessed row
            // fails loudly naming the ritual; a mismatching row is a
            // structural regression; a blessed scene that stops
            // completing is a regression too.
            if bless {
                let rows: std::collections::BTreeMap<String, String> =
                    completed.iter().cloned().collect();
                write_baselines(&rows);
                println!("corpus-bless   wrote {} baseline row(s)", rows.len());
            } else {
                for (scene, facts) in &completed {
                    match baselines.get(scene) {
                        Some(expected) if expected == facts => {
                            println!("corpus-locked  {scene} matches its baseline");
                        }
                        Some(expected) => panic!(
                            "structural baseline mismatch for {scene}\n  \
                             expected: {expected}\n  measured: {facts}\n\
                             (re-bless deliberately with FMN_CORPUS_BLESS=1 \
                             if the change is intended)"
                        ),
                        None => panic!(
                            "{scene} completed but has no committed baseline; \
                             bless it with: FMN_CORPUS_BLESS=1 cargo test -p \
                             fmn-python corpus"
                        ),
                    }
                }
                let completed_names: std::collections::HashSet<&str> =
                    completed.iter().map(|(scene, _)| scene.as_str()).collect();
                for scene in baselines.keys() {
                    assert!(
                        completed_names.contains(scene.as_str()),
                        "{scene} has a committed structural baseline but no \
                         longer completes — a bridge regression, not a \
                         worklist entry"
                    );
                }
            }

            // Determinism: the first completed scene, run twice
            // in-process, must reproduce identical structural facts.
            if let Some((scene, first_facts)) = completed.first() {
                let module_path = locked_seed()
                    .into_iter()
                    .find(|(name, _)| name == scene)
                    .expect("completed scene is in the seed")
                    .1;
                let (outcome, instance) = run_outcome(py, &module_path, scene);
                match outcome {
                    Outcome::Imported => {
                        let instance = instance.expect("completed run returns its instance");
                        let facts =
                            scene_facts(py, &instance).expect("structural facts of the rerun");
                        assert_eq!(
                            &facts, first_facts,
                            "{scene} is not deterministic across in-process runs"
                        );
                        println!("corpus-determinism {scene} reproduced its facts");
                    }
                    Outcome::Refused(reason) => {
                        panic!("{scene} completed once but refused on rerun: {reason}")
                    }
                }
            }
        });
    }

    fn put_videos_ref_on_sys_path(py: Python<'_>, videos_ref: &Path) {
        let sys_path = py
            .import("sys")
            .expect("sys")
            .getattr("path")
            .expect("sys.path");
        sys_path
            .call_method1(
                "insert",
                (
                    0,
                    PyString::new(py, videos_ref.to_str().expect("checkout path is UTF-8")),
                ),
            )
            .expect("front the pinned tree on sys.path");
        // Scene modules under era directories import their siblings as
        // top-level packages (`custom`, `manim_imports_ext`) — the tree
        // root is the one entry the shim adds; nothing else changes.
    }

    /// The lock parser and module-name mapping stay in lockstep with the
    /// committed VIDEO_CORPUS.lock shape.
    #[test]
    fn locked_seed_parses_and_maps_to_module_names() {
        for (scene, module_path) in locked_seed() {
            assert!(!scene.is_empty());
            assert!(module_path.ends_with(".py"));
            let name = module_path.trim_end_matches(".py").replace('/', ".");
            assert!(
                name.split('.').all(|part| !part.is_empty()),
                "module name well-formed: {name}"
            );
        }
    }
}
