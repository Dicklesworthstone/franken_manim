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
/// worklist; success means the scene is ready for structural assertions.
fn run_outcome(py: Python<'_>, module_path: &str, scene: &str) -> Outcome {
    let module_name = module_path.trim_end_matches(".py").replace('/', ".");
    let importlib = py.import("importlib").expect("importlib");
    let module = importlib
        .call_method1("import_module", (module_name.as_str(),))
        .expect("phase 2 runs only after a clean import");
    let class = module.getattr(scene).expect("locked scene class present");
    let outcome = class
        .call0()
        .and_then(|instance| instance.call_method0("run"));
    match outcome {
        Ok(_) => Outcome::Imported,
        Err(error) => Outcome::Refused(render_refusal(py, &error)),
    }
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
            // and drive the manim lifecycle. Refusals here are the
            // parity worklist (reported, precise, unasserted — the
            // frontier moves as the mobject/animation surface lands);
            // successes are scenes ready for structural assertions.
            for (scene, module_path) in locked_seed() {
                match run_outcome(py, &module_path, &scene) {
                    Outcome::Imported => {
                        println!("corpus-run    ok       {scene} ({module_path})");
                    }
                    Outcome::Refused(reason) => {
                        println!("corpus-run    frontier {scene} ({module_path}): {reason}");
                        assert!(
                            !reason.trim().is_empty()
                                && !reason.starts_with("UnknownExceptionType"),
                            "frontier must be a named error: {reason}"
                        );
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
