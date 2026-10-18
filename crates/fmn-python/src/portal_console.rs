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

#[cfg(test)]
mod tests {
    #[test]
    fn native_scene_console_acceptance() {
        assert_eq!(super::run_portal_gauntlet_console().unwrap(), 7);
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
