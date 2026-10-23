//! Construction-time Python ODE callbacks over Atlas's adaptive RK45 solver.
//! The solver owns integration and sampling; no scene or arena borrow crosses
//! an authored callback, and the GIL is released between callback evaluations.
use fmn_library::{IntegratorTune, OdeError, SamplingBudget, try_ode_solution_points};
use pyo3::exceptions::{PyTypeError, PyValueError};
use pyo3::prelude::*;
use pyo3::types::PyDict;

/// Bound the complete output, including higher-dimensional state vectors.
const OUTPUT_BUDGET: SamplingBudget = SamplingBudget::new(1_048_576);

#[pyfunction]
#[pyo3(signature = (function, state0, time, dt=0.01))]
pub(crate) fn _ode_solution_points(
    py: Python<'_>,
    function: Py<PyAny>,
    state0: &Bound<'_, PyAny>,
    time: f64,
    dt: f64,
) -> PyResult<Vec<Vec<f64>>> {
    if !function.bind(py).is_callable() {
        return Err(PyTypeError::new_err("ODE function must be callable"));
    }
    let asarray = py.import("numpy")?.getattr("asarray")?.unbind();
    let options = PyDict::new(py);
    options.set_item("dtype", "float64")?;
    let state = asarray.bind(py).call((state0,), Some(&options))?;
    if state.getattr("ndim")?.extract::<usize>()? != 1 {
        return Err(PyValueError::new_err(
            "ODE initial state must be a one-dimensional vector",
        ));
    }
    let initial: Vec<f64> = state.extract()?;
    let options = options.unbind();
    let result = py.detach(move || {
        try_ode_solution_points(
            |state| {
                Python::attach(|py| {
                    py.check_signals()?;
                    // A fresh ndarray exposes the normal Python callback
                    // surface without lending out the solver's state buffer.
                    let state = asarray
                        .bind(py)
                        .call((state.to_vec(),), Some(options.bind(py)))?;
                    let values = function.bind(py).call1((state,))?;
                    let values = asarray.bind(py).call((values,), Some(options.bind(py)))?;
                    if values.getattr("ndim")?.extract::<usize>()? != 1 {
                        return Err(PyValueError::new_err(
                            "ODE derivative must be a one-dimensional vector",
                        ));
                    }
                    values.extract::<Vec<f64>>()
                })
            },
            &initial,
            time,
            dt,
            // Use Atlas's visual-quality controls for sampled trajectories.
            // The coarse SciPy defaults bound a vector RMS local error and
            // can leave faster components noticeably less accurate.
            IntegratorTune::default(),
            OUTPUT_BUDGET,
        )
    });
    match result {
        Ok(solution) => Ok(solution.states),
        Err(OdeError::Callback(error)) => Err(error),
        Err(OdeError::Field(error)) => Err(PyValueError::new_err(error.to_string())),
    }
}

/// Exercise the public Python helper through the actual native solver.
///
/// # Errors
/// Returns the original Python/native acceptance failure.
#[cfg(any(test, feature = "gauntlet"))]
pub fn run_portal_gauntlet_ode_solution() -> Result<u64, String> {
    crate::with_python_test_module("native Python ODE trajectories", |py, _module, globals| {
        let code = std::ffi::CString::new(include_str!("../tests/native_ode_solution.py"))
            .map_err(|error| error.to_string())?;
        py.run(code.as_c_str(), Some(globals), Some(globals))
            .inspect_err(|error| error.print(py))
            .map_err(|error| error.to_string())?;
        globals
            .get_item("run_native_ode_solution")
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "native ODE acceptance entry point is absent".to_owned())?
            .call0()
            .inspect_err(|error| error.print(py))
            .and_then(|value| value.extract::<u64>())
            .map_err(|error| error.to_string())
    })
}

#[cfg(test)]
mod tests {
    #[test]
    fn python_callbacks_use_native_adaptive_ode_solver() {
        assert_eq!(super::run_portal_gauntlet_ode_solution().unwrap(), 8);
    }
}
