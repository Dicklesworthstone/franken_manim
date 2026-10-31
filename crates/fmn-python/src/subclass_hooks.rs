//! Library-class subclass hooks and authored easing, as embedded and wheel acceptance
//! (fm-5wq.13), including bare-scene clock configuration (fm-pr8c). These suites
//! also run through check_portal_runtime.sh against the installed wheel.

#[cfg(feature = "gauntlet")]
use pyo3::prelude::*;
#[cfg(feature = "gauntlet")]
use pyo3::types::PyDictMethods;

/// What the embedded portal observed.
#[cfg(feature = "gauntlet")]
#[derive(Debug)]
pub struct SubclassHookReport {
    /// Unittest cases run across the registered suites.
    pub cases: u64,
    /// Cases that failed or raised; zero on a pass.
    pub failed: u64,
    /// Library bases whose `init_points` override the matrix checked (Surface included).
    pub bases: u64,
}

/// Run library subclass/easing acceptance and the configured bare-scene clock
/// cases through the production portal.
///
/// # Errors
/// A suite that cannot load, or whose summary cannot be read. Failing cases are counted in
/// the report, not returned as an error, so the scenario logs them before it fails.
#[cfg(feature = "gauntlet")]
pub fn run_portal_gauntlet_subclass_hooks() -> Result<SubclassHookReport, String> {
    crate::with_python_test_module("library subclass hooks", |py, _module, globals| {
        for source in [
            include_str!("../tests/library_subclass_hooks.py"),
            include_str!("../tests/live_fade_rates.py"),
            include_str!("../tests/camera_configuration.py"),
        ] {
            let code = std::ffi::CString::new(source).map_err(|error| error.to_string())?;
            py.run(code.as_c_str(), Some(globals), Some(globals))
                .inspect_err(|error| error.print(py))
                .map_err(|error| error.to_string())?;
        }
        py.run(
            c"_suite = unittest.TestSuite(unittest.defaultTestLoader.loadTestsFromTestCase(case)\n\
              for case in (InitPointsOverrides, InterpolateOverride, LiveFadeRates))\n\
              _suite.addTests(CameraConfigurationTests(name) for name in (\n\
              'test_bare_runs_use_constructor_and_class_camera_fps',\n\
              'test_constructor_clock_is_ready_for_imperative_playback',\n\
              'test_observed_camera_dictionary_preserves_live_owners_then_collects',\n\
              'test_bare_clock_configuration_does_not_apply_a_recording_pixel_budget'))\n\
              _result = unittest.TextTestRunner(verbosity=2).run(_suite)\n\
              _summary = (_result.testsRun, len(_result.failures) + len(_result.errors), len(BASES) + 1)\n",
            Some(globals),
            Some(globals),
        )
        .inspect_err(|error| error.print(py))
        .map_err(|error| error.to_string())?;
        let (cases, failed, bases) = globals
            .get_item("_summary")
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "subclass hook summary is absent".to_owned())?
            .extract::<(u64, u64, u64)>()
            .map_err(|error| error.to_string())?;
        Ok(SubclassHookReport {
            cases,
            failed,
            bases,
        })
    })
}

#[cfg(test)]
mod tests {
    #[test]
    fn library_subclass_hooks_and_authored_easing() {
        crate::with_python_test_module("library subclass hooks", |py, _module, globals| {
            let source = std::ffi::CString::new(concat!(
                include_str!("../tests/library_subclass_hooks.py"),
                "\n",
                include_str!("../tests/live_fade_rates.py"),
                "\n",
                include_str!("../tests/camera_configuration.py"),
                "\n_suite = unittest.TestSuite(unittest.defaultTestLoader.loadTestsFromTestCase(case)",
                " for case in (InitPointsOverrides, InterpolateOverride, LiveFadeRates))\n",
                "_suite.addTests(CameraConfigurationTests(name) for name in (\n",
                "'test_bare_runs_use_constructor_and_class_camera_fps',\n",
                "'test_constructor_clock_is_ready_for_imperative_playback',\n",
                "'test_observed_camera_dictionary_preserves_live_owners_then_collects',\n",
                "'test_bare_clock_configuration_does_not_apply_a_recording_pixel_budget'))\n",
                "_result = unittest.TextTestRunner(verbosity=2).run(_suite)\n",
                "assert _result.wasSuccessful(), 'library subclass hook acceptance failed'\n",
            ))
            .unwrap();
            py.run(source.as_c_str(), Some(globals), Some(globals))
                .inspect_err(|error| error.print(py))
                .expect("library subclass hooks and authored easing");
        });
    }
}
