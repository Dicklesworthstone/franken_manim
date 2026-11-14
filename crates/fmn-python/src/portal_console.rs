//! Real native witnesses for checkpointed host scene editing (fm-ffj.70).

use pyo3::prelude::*;
use pyo3::types::PyDictMethods;

/// Exercise actual spawned-worker receipt delivery and shutdown, including
/// malformed/missing receipts. Native clip bytes have separate installed-wheel
/// coverage; this witness checks the transport that must preserve their rows.
///
/// # Errors
/// Returns the actual host-process test failure. Requires POSIX SIGINT.
#[cfg(feature = "gauntlet")]
pub fn run_portal_gauntlet_worker_receipt_shutdown() -> Result<u64, String> {
    crate::with_python_test_module("worker receipt shutdown", |py, _module, globals| {
        let tests = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests");
        globals
            .set_item("_FMN_WORKER_TESTS", tests.to_string_lossy().as_ref())
            .map_err(|error| error.to_string())?;
        let source = std::ffi::CString::new(
            r#"
import sys
import unittest
if _FMN_WORKER_TESTS not in sys.path:
    sys.path.insert(0, _FMN_WORKER_TESTS)
from test_process_batch import ProcessBatchTests
_worker_tests = unittest.TestSuite(ProcessBatchTests(name) for name in (
    "test_cancellation_keeps_receipt_before_stalled_optional_teardown",
    "test_non_json_receipt_is_failed_without_publication_claim",
    "test_keep_going_and_abrupt_worker_exit",
))
_worker_result = unittest.TextTestRunner(verbosity=2).run(_worker_tests)
assert _worker_result.testsRun == 3 and not _worker_result.skipped
assert _worker_result.wasSuccessful(), "real worker receipt shutdown tests failed"
"#,
        )
        .map_err(|error| error.to_string())?;
        py.run(source.as_c_str(), Some(globals), Some(globals))
            .inspect_err(|error| error.print(py))
            .map_err(|error| error.to_string())?;
        Ok(3)
    })
}

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
        // The per-option case reads both tables; the embedded run has no
        // __file__ to find them by.
        for (name, text) in [
            (
                "_FMN_CLI_FLAGS_MD",
                include_str!("../../../docs/api/cli_flags.md"),
            ),
            (
                "_FMN_BN15_MD",
                include_str!("../../../docs/behavior_notes/BN-15-cli-contract.md"),
            ),
        ] {
            globals
                .set_item(name, text)
                .map_err(|error| error.to_string())?;
        }
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

/// Exercise batch CLI options, ordinary checkpoint resume, and sequential
/// subdivided collections with native clips, final PNGs and integrity refusal.
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
import hashlib
from pathlib import Path
if _FMN_REPO_PYTHON not in sys.path:
    sys.path.insert(0, _FMN_REPO_PYTHON)
import manimlib
from fmn_python import RenderJob, render_scenes
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
    assert resumed.as_dict() == initial.as_dict()
    ordinary_checks = len(initial.outcomes)

    collection_calls = []
    blocked = [True]
    class Motion(manimlib.Scene):
        def __init__(self, label, **kwargs):
            collection_calls.append(label)
            if label == "second" and blocked[0]:
                raise RuntimeError("transient constructor failure")
            super().__init__(**kwargs)
        def construct(self):
            square = manimlib.Square(side_length=1, fill_opacity=1)
            self.add(square)
            self.wait(0.125)
            self.play(square.animate.shift(manimlib.RIGHT), run_time=0.25)
            self.wait(0.125)

    jobs = [RenderJob(name, Motion, {"label": name}) for name in ("first", "second")]
    opts = dict(format="y4m", subdivide=True, save_last_frame=True,
                resolution=(64, 36), fps=8, threads=1, continue_on_error=True,
                checkpoint=root / "collections.json", resume_key="collections-v1")
    initial = render_scenes(jobs, root / "collections", **opts)
    assert [row.status for row in initial.outcomes] == ["succeeded", "failed"]
    assert collection_calls == ["first", "second"]

    def snapshot_pair(pair):
        assert pair.completed and pair.primary.completed
        saved = {}
        for receipt in [segment.render for segment in pair.primary.segments] + [pair.still]:
            path = receipt.destination
            data = path.read_bytes()
            assert len(data) == receipt.bytes and hashlib.sha256(data).hexdigest() == receipt.digest
            assert data.startswith(b"YUV4MPEG2 ") if receipt.format == "y4m" else data.startswith(b"\x89PNG\r\n\x1a\n")
            saved[path] = (data, path.stat().st_mtime_ns)
        return saved

    saved = snapshot_pair(initial.outcomes[0].result)
    blocked[0] = False
    collection_calls.clear()
    resumed = render_scenes(jobs, root / "collections", resume=True, **opts)
    assert resumed.ok and collection_calls == ["second"]
    assert resumed.outcomes[0].as_dict() == initial.outcomes[0].as_dict()
    for path, snapshot in saved.items():
        assert (path.read_bytes(), path.stat().st_mtime_ns) == snapshot
    for row in resumed.outcomes:
        assert [segment.play_index for segment in row.result.primary.segments] == [0, 1, 2]
        snapshot_pair(row.result)
    collection_calls.clear()
    complete = render_scenes(jobs, root / "collections", resume=True, **opts)
    assert complete.as_dict() == resumed.as_dict() and collection_calls == []

    empty_opts = dict(opts, checkpoint=root / "empty-collections.json", animation_range=(9, None))
    empty = render_scenes(jobs, root / "empty-collections", **empty_opts)
    assert empty.ok and collection_calls == ["first", "second"]
    for row in empty.outcomes:
        assert row.result.primary.segments == ()
        snapshot_pair(row.result)
    collection_calls.clear()
    empty_resume = render_scenes(jobs, root / "empty-collections", resume=True, **empty_opts)
    assert empty_resume.as_dict() == empty.as_dict() and collection_calls == []

    damaged = initial.outcomes[0].result.primary.segments[0].render.destination
    changed = saved[damaged][0] + b"changed saved native clip"
    damaged.write_bytes(changed)
    try:
        render_scenes(jobs, root / "collections", resume=True, **opts)
    except ValueError:
        pass
    else:
        raise AssertionError("modified native clip was accepted for checkpoint reuse")
    assert collection_calls == [] and damaged.read_bytes() == changed
    return (ordinary_checks, len(complete.outcomes) + len(empty_resume.outcomes) + 1)
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
    fn native_documented_typography_configuration_effects() {
        crate::with_python_test_module("native typography config", |py, _module, globals| {
            let source = std::ffi::CString::new(include_str!("../tests/camera_configuration.py"))
                .expect("camera configuration tests contain no NUL");
            py.run(source.as_c_str(), Some(globals), Some(globals))
                .inspect_err(|error| error.print(py))
                .expect("load native configuration tests");
            py.run(
                c"suite = unittest.TestSuite(CameraConfigurationTests(name) for name in ('test_documented_typography_defaults_change_native_ink_and_survive_scope', 'test_character_template_cache_respects_scoped_typography_defaults', 'test_failed_configured_typography_import_restores_scope', 'test_explicit_typography_file_routes_output_with_cli_precedence'))\nresult = unittest.TextTestRunner(verbosity=2).run(suite)\nassert result.wasSuccessful(), 'native documented configuration effects failed'",
                Some(globals),
                Some(globals),
            )
            .inspect_err(|error| error.print(py))
            .expect("native documented configuration effects");
        });
    }

    #[test]
    fn native_scene_console_acceptance() {
        assert_eq!(super::run_portal_gauntlet_console().unwrap(), 8);
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
        assert_eq!(result.unwrap(), (2, 5));
    }

    #[test]
    fn native_reference_flags_acceptance() {
        assert_eq!(super::run_portal_gauntlet_reference_flags().unwrap(), 15);
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
for lower, upper in [
    ('text:\n  font: Computer Modern\ntex:\n  template: default\nfile_writer:\n  video_codec: libx264\n  pixel_format: yuv420p\n',
     'style:\n  font: IBM Plex Sans\n  tex_pack: basic\noutput:\n  vcodec: auto\n  pix_fmt: rgba\ndirectories:\n  output: configured-frames\n'),
    ('style:\n  font: Computer Modern\n  tex_pack: default\noutput:\n  vcodec: libx264\n  pix_fmt: yuv420p\n',
     'text:\n  font: IBM Plex Sans\ntex:\n  template: basic\nfile_writer:\n  video_codec: auto\n  pixel_format: rgba\ndirectories:\n  subdirs:\n    output: configured-frames\n'),
]:
    configured = manimlib._resolved_portal_config(lower, upper)
    for section in ('values', 'overrides', 'explicit_overrides'):
        value = configured[section]
        assert value['text']['font'] == 'IBM Plex Sans'
        assert value['tex']['template'] == 'basic'
        assert value['file_writer']['video_codec'] == 'auto'
        assert value['file_writer']['pixel_format'] == 'rgba'
        assert value['directories']['subdirs']['output'] == 'configured-frames'
        assert 'style' not in value and 'output' not in value
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
