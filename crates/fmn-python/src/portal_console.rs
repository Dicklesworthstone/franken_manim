//! Real native witnesses for checkpointed host scene editing (fm-ffj.70).

use pyo3::prelude::*;
use pyo3::types::PyDictMethods;

/// Exercise SceneState, callback identity, rational time and native readback.
///
/// # Errors
/// Returns the actual Python/native acceptance failure, never a ledger verdict.
pub fn run_portal_gauntlet_console() -> Result<u64, String> {
    crate::with_python_test_module("scene console", |py, _module, globals| {
        let source = std::ffi::CString::new(include_str!("../tests/scene_console_acceptance.py"))
            .map_err(|error| error.to_string())?;
        py.run(source.as_c_str(), Some(globals), Some(globals))
            .inspect_err(|error| error.print(py))
            .map_err(|error| error.to_string())?;
        globals
            .get_item("run_console_acceptance")
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "scene console acceptance entry point is absent".to_owned())?
            .call0()
            .inspect_err(|error| error.print(py))
            .and_then(|value| value.extract::<u64>())
            .map_err(|error| error.to_string())
    })
}

/// Exercise the public console path, Reference flags, real native outputs,
/// configured frame rates, and destination publication.
///
/// # Errors
/// Returns the actual Python/native acceptance failure.
pub fn run_portal_gauntlet_reference_flags() -> Result<u64, String> {
    crate::with_python_test_module("Reference console flags", |py, _module, globals| {
        let source = std::ffi::CString::new(include_str!("../tests/console_rendering.py"))
            .map_err(|error| error.to_string())?;
        py.run(source.as_c_str(), Some(globals), Some(globals))
            .inspect_err(|error| error.print(py))
            .map_err(|error| error.to_string())?;
        globals
            .get_item("CASES")
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "Reference console acceptance cases are absent".to_owned())?
            .len()
            .map(|count| count as u64)
            .map_err(|error| error.to_string())
    })
}

/// Exercise portal parallel-batch CLI option parsing and multi-scene checkpoint resume.
///
/// # Errors
/// Returns the actual Python/native acceptance failure.
pub fn run_portal_gauntlet_parallel_batch_checkpoint(
    root: &std::path::Path,
) -> Result<(u64, u64), String> {
    let root_str = root
        .to_str()
        .ok_or_else(|| "batch checkpoint root is not valid UTF-8".to_owned())?
        .to_owned();
    crate::with_python_test_module("parallel batch checkpoint", move |py, _module, globals| {
        let repo_python = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("python");
        globals
            .set_item("_FMN_REPO_PYTHON", repo_python.to_string_lossy().as_ref())
            .map_err(|error| error.to_string())?;
        globals
            .set_item("_FMN_BATCH_ROOT", &root_str)
            .map_err(|error| error.to_string())?;
        let script = r#"
import sys
from pathlib import Path
if _FMN_REPO_PYTHON not in sys.path:
    sys.path.insert(0, _FMN_REPO_PYTHON)
import manimlib
from fmn_python import render_scenes
from fmn_python.parallel_cli_options import take_parallel_options
from fmn_python.checkpoint_cli import take_checkpoint_options

def _run_parallel_batch_checkpoint_gauntlet():
    rem, par = take_parallel_options(
        ["scene.py", "--write_all", "--workers", "2", "--job-timeout", "15"],
        frozenset(),
    )
    assert rem == ["scene.py", "--write_all"] and par == {"workers": 2, "job_timeout": 15.0}
    rem2, chk = take_checkpoint_options(
        ["scene.py", "--write_all", "--checkpoint", "progress.json", "--resume-key", "e2e-v1", "--resume"],
        frozenset(),
    )
    assert rem2 == ["scene.py", "--write_all"]
    assert chk["resume"] is True and chk["resume_key"] == "e2e-v1"

    root = Path(_FMN_BATCH_ROOT)
    calls = []
    class First(manimlib.Scene):
        def construct(self):
            calls.append("first")
            self.add(manimlib.Square(side_length=1.0, fill_opacity=1.0))
    class Second(manimlib.Scene):
        def construct(self):
            calls.append("second")
            self.add(manimlib.Circle(radius=0.5, fill_opacity=1.0))

    jobs = {"first": First, "second": Second}
    opts = dict(
        format="png",
        resolution=(64, 36),
        fps=8,
        threads=1,
        checkpoint=root / "progress.json",
        resume_key="e2e-v1",
    )
    initial = render_scenes(jobs, root / "output", **opts)
    assert initial.ok and calls == ["first", "second"]
    calls.clear()
    resumed = render_scenes(jobs, root / "output", resume=True, **opts)
    assert resumed.ok and calls == []
    return (len(initial.outcomes), len(calls))
"#;
        let source = std::ffi::CString::new(script).map_err(|error| error.to_string())?;
        py.run(source.as_c_str(), Some(globals), Some(globals))
            .inspect_err(|error| error.print(py))
            .map_err(|error| error.to_string())?;
        globals
            .get_item("_run_parallel_batch_checkpoint_gauntlet")
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "parallel batch checkpoint entry point is absent".to_owned())?
            .call0()
            .inspect_err(|error| error.print(py))
            .and_then(|value| value.extract::<(u64, u64)>())
            .map_err(|error| error.to_string())
    })
}

#[cfg(test)]
mod tests {
    #[test]
    fn native_scene_console_acceptance() {
        assert_eq!(super::run_portal_gauntlet_console().unwrap(), 7);
    }

    #[test]
    fn parallel_batch_checkpoint_acceptance() {
        let root = std::env::temp_dir().join(format!(
            "fmn-portal-batch-checkpoint-test-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let result = super::run_portal_gauntlet_parallel_batch_checkpoint(&root);
        let _ = std::fs::remove_dir_all(&root);
        assert_eq!(result.unwrap(), (2, 0));
    }

    #[test]
    fn native_reference_flags_acceptance() {
        assert_eq!(super::run_portal_gauntlet_reference_flags().unwrap(), 14);
    }

    #[test]
    fn native_portal_config_layers_keep_types_and_precedence() {
        crate::with_python_test_module("console configuration", |py, _module, globals| {
            py.run(cr##"import manimlib
resolved = manimlib._resolved_portal_config(
    'camera:\n  fps: 10\n  background_color: "#112233"\n',
    'camera:\n  fps: 24\n  resolution: (120, 80)\nresolution_options:\n  low: (60, 40)\nauthored:\n  enabled: True\n  absent: null\n')
assert resolved['values']['camera']['fps'] == 24
assert resolved['values']['camera']['background_color'] == '#112233'
assert resolved['values']['camera']['resolution'] == (120, 80)
assert resolved['values']['resolution_options']['low'] == (60, 40)
assert resolved['overrides']['camera']['fps'] == 24
assert resolved['overrides']['authored'] == dict(enabled=True, absent=None)
assert 'text' not in resolved['overrides']
assert manimlib._portal_cli_color('red') == '#FF0000'
assert manimlib._portal_cli_color('RebeccAPurple') == '#663399'
assert manimlib._portal_cli_color('#abc') == '#AABBCC'
for value, expected in [('not-a-color', 'CSS color name')]:
    try:
        manimlib._portal_cli_color(value)
    except ValueError as error:
        assert expected in str(error), error
    else:
        raise AssertionError('invalid CLI color was accepted')
try:
    manimlib._resolved_portal_config(None, 'camera:\n  fps: nope\n')
except ValueError as error:
    assert 'camera.fps' in str(error), error
else:
    raise AssertionError('invalid typed configuration was accepted')
"##, Some(globals), Some(globals)).inspect_err(|error| error.print(py))
                .expect("native portal configuration resolution");
        });
    }
}
