//! Exact, bounded rate-function preparation on an owner's sampling schedule.
//!
//! A collector is deliberately effectful and cannot cross a pure-frame
//! boundary. Its owner runs the ordinary native animation driver against an
//! isolated stage, then replaces it with immutable tables. Tables compare the
//! complete binary64 input, including signed zero; they never interpolate.

use std::collections::BTreeMap;
use std::fmt;
use std::sync::{Arc, Mutex, OnceLock};

use crate::{AnimError, RateFunc};

fn failure(detail: impl Into<String>) -> AnimError {
    AnimError::UpdateFailed {
        operation: "exact rate preparation",
        detail: detail.into(),
    }
}

type Callback = dyn Fn(f64) -> Result<f64, String> + Send + Sync;

/// An owner-only collector for an explicitly declared pure scalar function.
///
/// The declaration belongs to the caller: this type does not infer purity by
/// probing a function. It invokes the callback once per distinct input in
/// first-use order and retains the first failure without further callbacks.
pub struct RateSampler {
    callback: Box<Callback>,
    samples: Mutex<BTreeMap<u64, f64>>,
    error: OnceLock<AnimError>,
    max_samples: usize,
}

impl fmt::Debug for RateSampler {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RateSampler")
            .field("max_samples", &self.max_samples)
            .field("error", &self.error.get())
            .finish_non_exhaustive()
    }
}

impl RateSampler {
    /// Create a bounded collector. Only the declared preparation owner may
    /// evaluate it; its Send/Sync bounds keep RateFunc's closed carriers
    /// transferable, not permission to execute host code on render workers.
    pub fn new(
        max_samples: usize,
        callback: impl Fn(f64) -> Result<f64, String> + Send + Sync + 'static,
    ) -> Arc<Self> {
        Arc::new(Self {
            callback: Box::new(callback),
            samples: Mutex::new(BTreeMap::new()),
            error: OnceLock::new(),
            max_samples,
        })
    }

    pub(crate) fn eval(&self, alpha: f64) -> f64 {
        if self.error.get().is_some() {
            return 0.0;
        }
        let value = (|| {
            if !alpha.is_finite() {
                return Err(failure("rate input must be finite"));
            }
            let samples = self
                .samples
                .lock()
                .map_err(|_| failure("sample collector lock poisoned"))?;
            if let Some(value) = samples.get(&alpha.to_bits()) {
                return Ok(*value);
            }
            if samples.len() >= self.max_samples {
                return Err(failure(format!(
                    "rate exceeds the {} distinct-sample budget",
                    self.max_samples
                )));
            }
            // No lock or arena borrow is lent to the authored function.
            drop(samples);
            let value = (self.callback)(alpha).map_err(failure)?;
            if !value.is_finite() {
                return Err(failure("rate function returned a non-finite value"));
            }
            self.samples
                .lock()
                .map_err(|_| failure("sample collector lock poisoned"))?
                .insert(alpha.to_bits(), value);
            Ok(value)
        })();
        match value {
            Ok(value) => value,
            Err(error) => {
                let _ = self.error.set(error);
                // Interpolation has an infallible f64 boundary. Keep its
                // private scratch finite until the driver's deferred-error
                // check; this value is never a successful rendered result.
                0.0
            }
        }
    }

    /// The original preparation error, if an interpolation recorded one.
    pub fn error(&self) -> Option<AnimError> {
        self.error.get().cloned()
    }

    /// Freeze exactly the observed arguments into a callback-free rate.
    ///
    /// # Errors
    /// Returns the first callback/budget failure. An unobserved table lookup
    /// later records a named error instead of inventing an interpolated value.
    pub fn finish(&self) -> Result<RateFunc, AnimError> {
        if let Some(error) = self.error() {
            return Err(error);
        }
        let samples = self
            .samples
            .lock()
            .map_err(|_| failure("sample collector lock poisoned"))?;
        Ok(RateFunc::Exact(Arc::new(ExactRateTable {
            samples: samples.iter().map(|(&key, &value)| (key, value)).collect(),
            error: OnceLock::new(),
        })))
    }
}

/// Immutable binary64 lookup table produced by an actual native rehearsal.
/// The error slot records a violated sampling contract; sample values never
/// mutate and no host callback or interpreter handle is retained.
#[derive(Debug)]
pub struct ExactRateTable {
    samples: Box<[(u64, f64)]>,
    error: OnceLock<AnimError>,
}

impl ExactRateTable {
    /// Number of distinct inputs observed during preparation.
    pub fn len(&self) -> usize {
        self.samples.len()
    }

    /// Whether the animation never needed this rate (for example an empty
    /// family whose interval is never entered).
    pub fn is_empty(&self) -> bool {
        self.samples.is_empty()
    }

    pub(crate) fn eval(&self, alpha: f64) -> f64 {
        match self
            .samples
            .binary_search_by_key(&alpha.to_bits(), |(key, _)| *key)
        {
            Ok(index) => self.samples[index].1,
            Err(_) => {
                let _ = self.error.set(failure(format!(
                    "unprepared rate input {alpha:?} (bits {:016x}); prepare again for this schedule",
                    alpha.to_bits()
                )));
                0.0
            }
        }
    }

    pub(crate) fn error(&self) -> Option<AnimError> {
        self.error.get().cloned()
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;

    #[test]
    fn preserves_discontinuities_signed_zero_and_first_use_order() {
        let observed = Arc::new(Mutex::new(Vec::new()));
        let captured = Arc::clone(&observed);
        let collector = RateSampler::new(8, move |alpha| {
            captured.lock().unwrap().push(alpha.to_bits());
            Ok(if alpha < 0.37 {
                0.0
            } else {
                alpha * alpha * alpha
            })
        });
        let raw = [0.0, 0.371, -0.0, 0.36, 0.371, 1.0];
        let expected: Vec<_> = raw.iter().map(|&a| collector.eval(a)).collect();
        let exact = collector.finish().unwrap();
        for (&a, &value) in raw.iter().zip(&expected) {
            assert_eq!(exact.eval(a).to_bits(), value.to_bits());
        }
        assert_eq!(
            *observed.lock().unwrap(),
            [0.0_f64, 0.371, -0.0, 0.36, 1.0].map(f64::to_bits)
        );
        let _ = exact.eval(0.365);
        assert!(
            exact
                .error()
                .unwrap()
                .to_string()
                .contains("unprepared rate input")
        );
    }

    #[test]
    fn first_failure_stops_callbacks_and_no_partial_table_escapes() {
        let calls = Arc::new(Mutex::new(0));
        let observed = Arc::clone(&calls);
        let collector = RateSampler::new(1, move |alpha| {
            *observed.lock().unwrap() += 1;
            Ok(alpha)
        });
        assert_eq!(collector.eval(0.0), 0.0);
        let _ = collector.eval(0.5);
        let _ = collector.eval(1.0);
        assert_eq!(*calls.lock().unwrap(), 1);
        assert!(
            collector
                .finish()
                .unwrap_err()
                .to_string()
                .contains("budget")
        );
        let nonfinite = RateSampler::new(10, |_| Ok(f64::INFINITY));
        let _ = nonfinite.eval(0.0);
        assert!(
            nonfinite
                .finish()
                .unwrap_err()
                .to_string()
                .contains("non-finite")
        );
    }
}
