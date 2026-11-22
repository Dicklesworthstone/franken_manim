//! Host-Python clip assembly through the ordinary native Reel facade.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use fmn::{FfmpegCapability, VideoConcatError, VideoConcatOptions};
use fmn_platform::fs::{FsError, StdFs};
use fmn_platform::process::{StdFfmpegLocator, StdProcessRunner};
use pyo3::exceptions::{
    PyFileExistsError, PyFileNotFoundError, PyOSError, PyRuntimeError, PyTypeError, PyValueError,
};
use pyo3::prelude::*;
use pyo3::types::{PyBool, PyDict, PyList, PyModule, PyTuple};

const POLICY_KEYS: [&str; 6] = [
    "max_clips",
    "max_clip_bytes",
    "max_input_bytes",
    "timeout",
    "max_log_bytes",
    "max_output_bytes",
];

pub(crate) fn install_concatenation(module: &Bound<'_, PyModule>) -> PyResult<()> {
    // A private host boundary: do not expand the Reference wildcard surface.
    module.setattr(
        "_portal_concatenate_video",
        wrap_pyfunction!(_portal_concatenate_video, module)?,
    )
}

fn positive_policy(policy: &Bound<'_, PyDict>, name: &str) -> PyResult<u64> {
    let value = policy
        .get_item(name)?
        .ok_or_else(|| PyValueError::new_err(format!("missing video composition policy {name}")))?;
    if value.is_instance_of::<PyBool>() {
        return Err(PyTypeError::new_err(format!(
            "{name} must be a positive integer, not bool"
        )));
    }
    let value = value.extract::<u64>()?;
    if value == 0 {
        return Err(PyValueError::new_err(format!("{name} must be nonzero")));
    }
    Ok(value)
}

fn text_path(value: &str, name: &str) -> PyResult<PathBuf> {
    if value.is_empty() || value.contains('\0') {
        return Err(PyValueError::new_err(format!(
            "{name} must be a nonempty text path without NUL"
        )));
    }
    Ok(PathBuf::from(value))
}

fn concat_error(error: VideoConcatError) -> PyErr {
    let message = error.to_string();
    match error {
        VideoConcatError::InvalidOptions(_)
        | VideoConcatError::EmptyInput { .. }
        | VideoConcatError::Input {
            error: FsError::TooLarge { .. } | FsError::TooManyEntries { .. },
            ..
        } => PyValueError::new_err(message),
        VideoConcatError::DestinationExists(_) => PyFileExistsError::new_err(message),
        VideoConcatError::Input {
            error: FsError::NotFound { .. },
            ..
        } => PyFileNotFoundError::new_err(message),
        VideoConcatError::Input { .. } | VideoConcatError::Output(_) => PyOSError::new_err(message),
        VideoConcatError::Capability(_) | VideoConcatError::FfmpegUnavailable(_) => {
            crate::CapabilityError::new_err(message)
        }
        VideoConcatError::Allocation(_) | VideoConcatError::Boundary(_) => {
            PyRuntimeError::new_err(message)
        }
    }
}

#[pyfunction]
#[pyo3(signature = (inputs, destination, ffmpeg_bin, policy, workdir=None))]
fn _portal_concatenate_video(
    py: Python<'_>,
    inputs: &Bound<'_, PyTuple>,
    destination: &str,
    ffmpeg_bin: &str,
    policy: &Bound<'_, PyDict>,
    workdir: Option<&str>,
) -> PyResult<Py<PyDict>> {
    let cwd = std::env::current_dir().map_err(|error| PyOSError::new_err(error.to_string()))?;
    if py
        .import("fmn_python.effect_audit")?
        .getattr("certification_active")?
        .call0()?
        .extract::<bool>()?
    {
        return Err(crate::CapabilityError::new_err(
            "encoded video composition is outside the certified artifact set; finish the certified capture before assembling clips",
        ));
    }
    if policy.len() != POLICY_KEYS.len()
        || policy
            .keys()
            .iter()
            .any(|key| match key.extract::<String>() {
                Ok(key) => !POLICY_KEYS.contains(&key.as_str()),
                Err(_) => true,
            })
    {
        return Err(PyValueError::new_err(
            "unknown or missing video composition policy",
        ));
    }
    let mut options = VideoConcatOptions::new(cwd.join(text_path(destination, "destination")?));
    options.max_inputs = usize::try_from(positive_policy(policy, "max_clips")?)
        .map_err(|_| PyValueError::new_err("max_clips exceeds the host addressable range"))?;
    if options.max_inputs > fmn_output::ffmpeg::MAX_CONCAT_INPUTS
        || inputs.is_empty()
        || inputs.len() > options.max_inputs
    {
        return Err(PyValueError::new_err(
            "video composition requires a nonempty input tuple within max_clips and the native clip ceiling",
        ));
    }
    options.max_input_bytes = positive_policy(policy, "max_clip_bytes")?;
    options.max_total_input_bytes = positive_policy(policy, "max_input_bytes")?;
    options.ffmpeg_limits.timeout = Duration::from_secs(positive_policy(policy, "timeout")?);
    options.ffmpeg_limits.max_log_bytes = positive_policy(policy, "max_log_bytes")?;
    options.ffmpeg_limits.max_artifact_bytes = positive_policy(policy, "max_output_bytes")?;
    options.ffmpeg_bin = text_path(ffmpeg_bin, "ffmpeg_bin")?;
    if options.ffmpeg_bin.components().count() > 1 {
        options.ffmpeg_bin = cwd.join(&options.ffmpeg_bin);
    }
    let mut paths = Vec::new();
    paths
        .try_reserve_exact(inputs.len())
        .map_err(|_| PyRuntimeError::new_err("could not reserve bounded video input paths"))?;
    for value in inputs.iter() {
        paths.push(cwd.join(text_path(&value.extract::<String>()?, "input")?));
    }
    let workdir_root = match workdir {
        Some(value) => cwd.join(text_path(value, "workdir")?),
        None => std::env::temp_dir(),
    };
    // Host audit hooks, including certified-scene effect recording, must see
    // this explicit external-media operation even though its I/O is native.
    py.import("sys")?.call_method1(
        "audit",
        ("fmn.video_concat", inputs, destination, ffmpeg_bin),
    )?;
    options.ffmpeg = Some(FfmpegCapability {
        runner: Arc::new(StdProcessRunner),
        locator: Arc::new(StdFfmpegLocator::from_host_path()),
        workdir_root,
    });
    let max_clips = options.max_inputs;
    let max_clip_bytes = options.max_input_bytes;
    let max_input_bytes = options.max_total_input_bytes;
    // No Python reference or Scene borrow crosses the GIL-free filesystem,
    // bounded process wait or atomic publication.
    let report = py
        .detach(move || fmn::concatenate_video_with_fs(&paths, options, Arc::new(StdFs)))
        .map_err(concat_error)?;
    let result = PyDict::new(py);
    result.set_item("schema", "fmn.video_concat.v1")?;
    result.set_item("destination", report.boundary.destination)?;
    result.set_item("bytes", report.boundary.artifact_bytes)?;
    result.set_item("digest", report.boundary.artifact_digest.to_hex())?;
    result.set_item("total_input_bytes", report.total_input_bytes)?;
    result.set_item("certified", false)?;
    let clips = PyList::empty(py);
    for input in report.inputs {
        let clip = PyDict::new(py);
        clip.set_item("source", input.source_path)?;
        clip.set_item("bytes", input.bytes)?;
        clip.set_item("digest", input.digest.to_hex())?;
        clips.append(clip)?;
    }
    result.set_item("inputs", clips)?;
    result.set_item(
        "ffmpeg_invocations",
        crate::ffmpeg_invocation_facts(py, &report.boundary.invocations)?,
    )?;
    let limits = PyDict::new(py);
    limits.set_item("timeout_seconds", report.limits.timeout.as_secs())?;
    limits.set_item("timeout_nanoseconds", report.limits.timeout.subsec_nanos())?;
    limits.set_item("max_log_bytes", report.limits.max_log_bytes)?;
    limits.set_item("max_artifact_bytes", report.limits.max_artifact_bytes)?;
    limits.set_item("max_input_bytes", report.limits.max_input_bytes)?;
    limits.set_item("keep_workdir", report.limits.keep_workdir)?;
    result.set_item("ffmpeg_limits", limits)?;
    // Never re-read the caller's mutable dict after releasing the GIL.
    let source_limits = PyDict::new(py);
    source_limits.set_item("max_clips", max_clips)?;
    source_limits.set_item("max_clip_bytes", max_clip_bytes)?;
    source_limits.set_item("max_input_bytes", max_input_bytes)?;
    result.set_item("source_limits", source_limits)?;
    Ok(result.unbind())
}

/// Exercise the public Python facade through the native process fixture.
#[cfg(any(test, feature = "gauntlet"))]
pub fn run_portal_gauntlet_video_concat(
    executable: &std::path::Path,
    directory: &std::path::Path,
) -> Result<u64, String> {
    crate::with_python_test_module("portal video composition", |py, _module, globals| {
        let source = std::ffi::CString::new(include_str!("../../tests/video_composition.py"))
            .expect("video composition suite contains no NUL");
        py.run(source.as_c_str(), Some(globals), Some(globals))
            .inspect_err(|error| error.print(py))
            .map_err(|error| error.to_string())?;
        globals
            .get_item("run_video_composition_acceptance")
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "video composition suite has no entry point".to_owned())?
            .call1((executable.to_path_buf(), directory.to_path_buf()))
            .inspect_err(|error| error.print(py))
            .and_then(|value| value.extract::<u64>())
            .map_err(|error| error.to_string())
    })
}
