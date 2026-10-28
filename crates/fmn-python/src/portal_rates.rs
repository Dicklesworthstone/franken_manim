//! Explicit pure-rate preparation using Choreo's actual native sample driver.
//! No alternate frame grid or lag/time-span arithmetic lives in the portal.

use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, Weak};

use fmn_anim::rate_table::RateSampler;
use fmn_anim::{AnimationSignature, RateFunc, RationalFrameClock};
use pyo3::exceptions::{PyRuntimeError, PyTypeError, PyValueError};
use pyo3::gc::{PyTraverseError, PyVisit};
use pyo3::prelude::*;

use crate::{AnimSpec, Engine, anim_error, build_native_animation, rate_func_from_py};

const MAX_SAMPLES: usize = 1_048_576;
const MAX_PREPARATION_FRAMES: i64 = 1_048_576;

/// Explicitly declare a rate function to depend only on its finite alpha.
///
/// Import as `from fmn_python import PureRateFunction`. The callable must be
/// deterministic and side-effect-free: it must not read or mutate a scene,
/// clock, RNG, closure counter, or other changing state. A native rehearsal
/// evaluates each distinct argument once, then renders from exact tables.
/// Ordinary callables retain their existing per-frame Python semantics.
#[pyclass(name = "_PureRateFunction")]
pub(crate) struct PyPureRateFunction {
    function: Option<Py<PyAny>>,
}

#[pymethods]
impl PyPureRateFunction {
    #[new]
    fn new(py: Python<'_>, function: Py<PyAny>) -> PyResult<Self> {
        if !function.bind(py).is_callable() {
            return Err(PyTypeError::new_err("PureRateFunction requires a callable"));
        }
        Ok(Self {
            function: Some(function),
        })
    }

    #[getter]
    fn function(&self, py: Python<'_>) -> PyResult<Py<PyAny>> {
        self.function
            .as_ref()
            .map(|value| value.clone_ref(py))
            .ok_or_else(|| {
                PyRuntimeError::new_err("PureRateFunction has been cleared by garbage collection")
            })
    }

    fn __call__(&self, py: Python<'_>, alpha: f64) -> PyResult<Py<PyAny>> {
        self.function(py)?.call1(py, (alpha,))
    }

    fn __traverse__(&self, visit: PyVisit<'_>) -> Result<(), PyTraverseError> {
        visit.call(&self.function)
    }

    fn __clear__(&mut self) {
        self.function = None;
    }
}

struct Collector {
    sampler: Arc<RateSampler>,
}

/// A whole play shares one failure boundary, including native finish passes.
/// Registry entries are weak: a sampler's callback retains this state without
/// making a reference cycle. No lock is held while authored Python executes.
#[derive(Default)]
struct PreparationState {
    first_error: Mutex<Option<PyErr>>,
    samplers: Mutex<Vec<Weak<RateSampler>>>,
    sample_count: AtomicUsize,
}

impl PreparationState {
    fn retain(&self, error: PyErr) -> Result<(), String> {
        let mut first = self
            .first_error
            .lock()
            .map_err(|_| "pure rate error lock poisoned".to_owned())?;
        if first.is_none() {
            *first = Some(error);
        }
        Ok(())
    }

    fn check_before_callback(&self) -> Result<(), String> {
        const STOPPED: &str = "declared pure rate preparation stopped after its first failure";
        if self
            .first_error
            .lock()
            .map_err(|_| "pure rate error lock poisoned".to_owned())?
            .is_some()
        {
            return Err(STOPPED.to_owned());
        }
        // A sampler can reject a nonfinite input or its own sample budget
        // before reaching this closure. Observe that failure before another
        // declaration invokes Python; cleanup must not replace its cause.
        let native_error = self
            .samplers
            .lock()
            .map_err(|_| "pure rate collector registry lock poisoned".to_owned())?
            .iter()
            .filter_map(Weak::upgrade)
            .find_map(|sampler| sampler.error());
        if let Some(error) = native_error {
            self.retain(anim_error(error))?;
            return Err(STOPPED.to_owned());
        }
        Ok(())
    }
}

/// One play owns its collectors. Reusing a declaration in another play always
/// prepares a fresh table for that play's actual fps, timing and family shape.
#[derive(Default)]
pub(super) struct Preparation {
    collectors: Vec<Collector>,
    identities: HashMap<usize, usize>,
    shared: Arc<PreparationState>,
}

impl Preparation {
    pub(super) fn is_empty(&self) -> bool {
        self.collectors.is_empty()
    }

    pub(super) fn parse(&mut self, value: &Bound<'_, PyAny>) -> PyResult<RateFunc> {
        let Ok(declaration) = value.cast::<PyPureRateFunction>() else {
            return rate_func_from_py(value);
        };
        let key = value.as_ptr() as usize;
        if let Some(&index) = self.identities.get(&key) {
            return Ok(RateFunc::Preparing(Arc::clone(
                &self.collectors[index].sampler,
            )));
        }
        let function = declaration.borrow().function(value.py())?;
        let shared = Arc::clone(&self.shared);
        let sampler = RateSampler::new(MAX_SAMPLES, move |alpha| {
            shared.check_before_callback()?;
            if shared.sample_count.fetch_add(1, Ordering::Relaxed) >= MAX_SAMPLES {
                let message = "play exceeds the 1048576 distinct-rate-sample budget";
                shared.retain(PyValueError::new_err(message))?;
                return Err(message.to_owned());
            }
            Python::attach(|py| {
                let result = py
                    .check_signals()
                    .and_then(|()| function.call1(py, (alpha,))?.extract::<f64>(py));
                match result {
                    Ok(value) if value.is_finite() => Ok(value),
                    Ok(_) => {
                        let message = "rate function returned a non-finite value";
                        shared.retain(PyValueError::new_err(message))?;
                        Err(message.to_owned())
                    }
                    Err(error) => {
                        // Preserve the initiating exception object/traceback.
                        // Never invoke its potentially authored __str__ here.
                        shared.retain(error)?;
                        Err("declared pure Python rate failed".to_owned())
                    }
                }
            })
        });
        self.shared
            .samplers
            .lock()
            .map_err(|_| PyRuntimeError::new_err("pure rate collector registry lock poisoned"))?
            .push(Arc::downgrade(&sampler));
        let rate = RateFunc::Preparing(Arc::clone(&sampler));
        self.identities.insert(key, self.collectors.len());
        self.collectors.push(Collector { sampler });
        Ok(rate)
    }

    pub(super) fn refusal() -> PyErr {
        PyValueError::new_err(
            "PureRateFunction preparation requires a callback-free native play with no \
             scene/mobject updaters or camera track; pass its .function for ordinary Python playback",
        )
    }

    pub(super) fn inputs(specs: &[AnimSpec]) -> Vec<fmn_mobject::Mob> {
        let mut inputs = Vec::new();
        for spec in specs {
            inputs.extend(spec.mob);
            inputs.extend(spec.target);
            inputs.extend(spec.mobs.iter().copied());
            inputs.extend(spec.pairs.iter().flat_map(|&(a, b)| [a, b]));
            inputs.extend(spec.source_keys.iter().map(|&(mob, _)| mob));
            inputs.extend(spec.target_keys.iter().map(|&(mob, _)| mob));
            inputs.extend(Self::inputs(&spec.members));
        }
        inputs
    }

    /// Rehearse only the native lifecycle on a private CoW stage. Its exact
    /// sample sequence includes begin, overshooting final frames, nested
    /// compositions and each animation's final_alpha_value. Nothing from the
    /// rehearsal is installed into the live scene, and no output is emitted.
    pub(super) fn prepare(
        &self,
        engine: &Engine,
        specs: &mut [AnimSpec],
        overrides: &mut fmn_scene::PlayOverrides,
    ) -> PyResult<usize> {
        let (mut stage, fps, time, allows_pure) = {
            let scene = engine.borrow();
            (
                scene.stage().snapshot().materialize(),
                scene.fps(),
                scene.time(),
                scene.allows_pure_segments(),
            )
        };
        if !allows_pure {
            return Err(Self::refusal());
        }
        stage.set_time_from_clock(time.to_f64());
        let mut animations = specs
            .iter()
            .cloned()
            .map(|spec| build_native_animation(&mut stage, spec))
            .collect::<PyResult<Vec<_>>>()?;
        for animation in &mut animations {
            animation.update_rate_info(
                overrides.run_time,
                overrides.rate_func.clone(),
                overrides.lag_ratio,
            );
            if animation.effect_signature() != AnimationSignature::Pure {
                return Err(Self::refusal());
            }
        }
        // Even an unrooted target's updater is an authored effect at begin or
        // finish. Check all native inputs before invoking any rate callback.
        for root in stage.roots().iter().copied().chain(
            animations
                .iter()
                .flat_map(|animation| animation.preflight_mobjects()),
        ) {
            if stage.family_updater_kinds(root) != (0, 0) {
                return Err(Self::refusal());
            }
        }
        let mut clock = RationalFrameClock::new(fps)
            .map_err(|error| PyValueError::new_err(error.to_string()))?;
        clock
            .advance_frames(time.frames())
            .map_err(|error| PyValueError::new_err(error.to_string()))?;
        let duration = animations
            .iter()
            .map(|animation| animation.get_run_time())
            .fold(f64::NEG_INFINITY, f64::max);
        let grid = clock
            .segment(duration)
            .map_err(|error| PyValueError::new_err(error.to_string()))?;
        if grid.n_frames() > MAX_PREPARATION_FRAMES {
            return Err(PyValueError::new_err(
                "PureRateFunction exceeds the 1048576-frame preparation budget",
            ));
        }
        // Use the native stepped lifecycle with capture disabled. On a body
        // failure abort instead of running an endpoint/cleanup rehearsal:
        // a later finish callback must never replace the initiating error.
        let rng = fmn_core::rng::RngRoot::from_seed(0);
        let result = fmn_anim::open_play_with_mode(&mut stage, &clock, &mut animations, true)
            .and_then(|mut open| {
                let body = (|| {
                    while fmn_anim::prepare_play_frame(
                        &mut stage,
                        &mut clock,
                        &mut animations,
                        &mut open,
                    )?
                    .is_some()
                    {
                        fmn_anim::complete_play_frame(
                            &mut stage,
                            &clock,
                            &rng,
                            &mut open,
                            false,
                            &mut |_, _| {},
                            &mut |_| {},
                        )?;
                    }
                    Ok(())
                })();
                match body {
                    Ok(()) => fmn_anim::finish_open_play(&mut stage, &mut animations, open),
                    Err(error) => {
                        let _ = fmn_anim::abort_open_play(&mut stage, &mut animations, open);
                        Err(error)
                    }
                }
            });
        if let Some(error) = self
            .shared
            .first_error
            .lock()
            .map_err(|_| PyRuntimeError::new_err("pure rate error lock poisoned"))?
            .take()
        {
            return Err(error);
        }
        for collector in &self.collectors {
            if let Some(error) = collector.sampler.error() {
                return Err(anim_error(error));
            }
        }
        result.map_err(anim_error)?;
        let mut tables = HashMap::new();
        let mut count = 0;
        for collector in &self.collectors {
            let rate = collector.sampler.finish().map_err(anim_error)?;
            if let RateFunc::Exact(table) = &rate {
                count += table.len();
            }
            tables.insert(Arc::as_ptr(&collector.sampler) as usize, rate);
        }
        fn replace(rate: &mut Option<RateFunc>, tables: &HashMap<usize, RateFunc>) {
            if let Some(RateFunc::Preparing(sampler)) = rate {
                *rate = Some(tables[&(Arc::as_ptr(sampler) as usize)].clone());
            }
        }
        fn walk(specs: &mut [AnimSpec], tables: &HashMap<usize, RateFunc>) {
            for spec in specs {
                replace(&mut spec.rate, tables);
                walk(&mut spec.members, tables);
            }
        }
        replace(&mut overrides.rate_func, &tables);
        walk(specs, &tables);
        Ok(count)
    }
}

/// Compare actual native/offloaded and live-Python renders, preserving PNGs.
/// Returns compared frames, distinct rate samples, and logical table bytes.
///
/// # Errors
/// Returns the original Python/native acceptance failure.
#[cfg(feature = "gauntlet")]
pub fn run_portal_gauntlet_exact_rates(root: &std::path::Path) -> Result<(u64, u64, u64), String> {
    run_exact_rate_acceptance(Some(root))
}

#[cfg(any(test, feature = "gauntlet"))]
fn run_exact_rate_acceptance(root: Option<&std::path::Path>) -> Result<(u64, u64, u64), String> {
    crate::with_python_test_module("exact pure Python rates", |py, _module, globals| {
        let code = std::ffi::CString::new(include_str!("../tests/exact_rate_offload.py"))
            .map_err(|error| error.to_string())?;
        py.run(&code, Some(globals), Some(globals))
            .inspect_err(|error| error.print(py))
            .map_err(|error| error.to_string())?;
        globals
            .get_item("run_exact_rate_acceptance")
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "missing exact-rate acceptance function".to_owned())?
            .call1((root.map(std::path::Path::to_path_buf),))
            .inspect_err(|error| error.print(py))
            .and_then(|result| result.extract())
            .map_err(|error| error.to_string())
    })
}

#[cfg(test)]
mod tests {
    #[test]
    fn declared_rates_match_live_python_at_exact_native_samples() {
        super::run_exact_rate_acceptance(None).unwrap();
    }
}
