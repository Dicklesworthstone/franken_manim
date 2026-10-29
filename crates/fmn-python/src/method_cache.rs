//! fm-zoi method-resolution cache: resolve each `(type, method)` once,
//! reuse the callable, invalidate on class mutation (§15.2 Rev 4).
//!
//! Invalidation mechanism — the boring one CPython itself uses. Every type
//! carries `tp_version_tag`; `PyType_Modified` (invoked on any class
//! mutation, including monkey-patching a method) zeroes the tag and
//! recursively invalidates every subclass, so the *leaf* type's tag covers
//! MRO mutation of bases. An entry is valid iff the leaf type's current tag
//! equals the tag recorded at resolution. Tag `0` is CPython's "invalid"
//! sentinel and never matches, so a mid-mutation read degrades to a
//! re-resolve, never a stale hit. A tag can only collide after exactly 2³²
//! intervening type mutations on a recycled type object — the same residual
//! risk CPython accepts for its own caches. The cache holds the keyed type
//! only weakly, and a hit requires that weak reference to name the very type
//! being looked up, so a recycled pointer can never reuse an entry while the
//! cache itself never keeps a dropped class alive (ADR-0015, Amendment 4).
//!
//! Correctness envelope (documented semantics):
//! - Resolution goes through `type.__getattribute__` (`PyType::getattr`), so
//!   the cached object is exactly what an unbound class attribute lookup
//!   yields — for plain functions and PyO3 methods, calling it with the
//!   instance as first argument is CPython's unbound-call semantics.
//! - Instance-`__dict__` shadowing is honored: if the instance carries the
//!   name in its own dict, the caller falls back to the ordinary
//!   `call_method` path. The cached path is for instance-method dispatch
//!   only (not staticmethod/classmethod contracts).
//!
//! The single `unsafe` read of `tp_version_tag` is the third ratified
//! project-authored unsafe item in this crate (ADR-0015, Amendment 1): a
//! read-only observation of CPython's own invalidation counter through
//! pyo3-ffi's non-limited-API `PyTypeObject` layout. It publishes no
//! pointer, extends no lifetime, and mutates nothing.
//!
//! Storage is thread-local: scene workers are single-threaded by ADR-0015,
//! so no synchronization exists on the hit path.

use std::cell::RefCell;
use std::collections::HashMap;

use pyo3::intern;
use pyo3::prelude::*;
use pyo3::types::{PyDict, PyTuple, PyType, PyWeakrefMethods, PyWeakrefReference};

/// One resolved `(type, method)` pair plus the tag it was resolved under.
///
/// Nothing the program can drop is held strongly: a strong type reference
/// pinned every instantiated subclass, and whatever its methods close over,
/// for the life of the process (fm-5wq.66).
struct CacheEntry {
    version_tag: u32,
    callable: Held,
    /// The keyed type, weakly. A hit requires it alive and identical to the
    /// looked-up type.
    owner: Py<PyWeakrefReference>,
}

/// A cached callable: weak when Python allows it. CPython's native callables
/// and descriptors cannot be weakly referenced, but they reference only their
/// own native owner, so they are held strongly.
enum Held {
    Weak(Py<PyWeakrefReference>),
    Native(Py<PyAny>),
}

impl Held {
    fn get<'py>(&self, py: Python<'py>) -> Option<Bound<'py, PyAny>> {
        match self {
            Self::Weak(reference) => reference.bind(py).upgrade(),
            Self::Native(callable) => Some(callable.bind(py).clone()),
        }
    }

    fn alive(&self, py: Python<'_>) -> bool {
        self.get(py).is_some()
    }
}

/// Type names of CPython's native callables and method descriptors.
const NATIVE_CALLABLES: [&str; 5] = [
    "builtin_function_or_method",
    "method_descriptor",
    "wrapper_descriptor",
    "method-wrapper",
    "classmethod_descriptor",
];

/// How the cache may hold `callable`, or `None` when it can be neither
/// weakly referenced nor safely pinned (a `property`, say, whose getter may
/// close over instances). Such a resolution is simply not cached.
fn hold(callable: &Bound<'_, PyAny>) -> Option<Held> {
    if let Ok(reference) = PyWeakrefReference::new(callable) {
        return Some(Held::Weak(reference.unbind()));
    }
    let kind = callable.get_type().name().ok()?;
    let kind = kind.to_str().ok()?;
    NATIVE_CALLABLES
        .contains(&kind)
        .then(|| Held::Native(callable.clone().unbind()))
}

/// Entries kept before the first sweep of dead ones; a sweep then waits until
/// the cache has doubled, so sweeping stays amortized O(1) per insertion.
const SWEEP_FLOOR: usize = 1024;

struct Cache {
    entries: HashMap<(usize, &'static str), CacheEntry>,
    next_sweep: usize,
    hits: u64,
    misses: u64,
    invalidations: u64,
}

impl Default for Cache {
    fn default() -> Self {
        Self {
            entries: HashMap::new(),
            next_sweep: SWEEP_FLOOR,
            hits: 0,
            misses: 0,
            invalidations: 0,
        }
    }
}

impl Cache {
    /// Remove entries whose type or callable has died, returning them so the
    /// caller drops them after releasing the cache borrow.
    fn sweep(&mut self, py: Python<'_>) -> Vec<CacheEntry> {
        let dead: Vec<_> = self
            .entries
            .iter()
            .filter(|(_, entry)| {
                entry.owner.bind(py).upgrade().is_none() || !entry.callable.alive(py)
            })
            .map(|(key, _)| *key)
            .collect();
        let removed = dead
            .iter()
            .filter_map(|key| self.entries.remove(key))
            .collect();
        self.next_sweep = SWEEP_FLOOR.max(2 * self.entries.len());
        removed
    }
}

thread_local! {
    static CACHE: RefCell<Cache> = RefCell::new(Cache::default());
}

/// Read-only observer statistics for tests and the detection report.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct MethodCacheStats {
    pub entries: u64,
    pub hits: u64,
    pub misses: u64,
    pub invalidations: u64,
}

/// The leaf type's current version tag; `0` means "invalid" per CPython.
fn type_version_tag(ty: &Bound<'_, PyType>) -> u32 {
    // SAFETY: `ty` is a live `PyType`, so its pointer addresses a valid
    // `PyTypeObject` for the duration of the borrow. ADR-0015 pins abi3 off,
    // so pyo3-ffi's full `#[repr(C)]` PyTypeObject layout is the interpreter
    // ABI; this reads one scalar field and mutates nothing.
    unsafe { (*ty.as_type_ptr()).tp_version_tag }
}

/// Resolve `(type, name)` through the cache. A miss or tag mismatch
/// re-resolves through the full Python attribute machinery.
fn resolve(ty: &Bound<'_, PyType>, name: &'static str) -> PyResult<Py<PyAny>> {
    let py = ty.py();
    let key = (ty.as_type_ptr() as usize, name);
    let tag = type_version_tag(ty);
    let (cached, stale) = CACHE.with(|cache| {
        let mut cache = cache.borrow_mut();
        // Some(Some(callable)): a valid hit. Some(None): a stale entry (a
        // changed tag, a dead or different type, or a dead callable).
        let lookup = cache.entries.get(&key).map(|entry| {
            let current = entry.version_tag == tag && tag != 0;
            let same_type = entry
                .owner
                .bind(py)
                .upgrade()
                .is_some_and(|owner| owner.is(ty));
            if current && same_type {
                entry.callable.get(py)
            } else {
                None
            }
        });
        match lookup {
            Some(Some(callable)) => {
                cache.hits += 1;
                (Some(callable.unbind()), None)
            }
            Some(None) => {
                cache.invalidations += 1;
                (None, cache.entries.remove(&key))
            }
            None => (None, None),
        }
    });
    // A decref may run Python code that reenters this cache, so stale entries
    // are dropped only after the borrow is released.
    drop(stale);
    if let Some(callable) = cached {
        return Ok(callable);
    }
    let callable = ty.getattr(name)?;
    // Re-read after resolution: the lookup itself can assign a fresh tag to
    // a type that was mid-mutation. A still-zero tag is stored as-is and
    // simply never hits — correct, just uncached.
    let tag = type_version_tag(ty);
    let entry = match (hold(&callable), PyWeakrefReference::new(ty.as_any())) {
        (Some(held), Ok(owner)) => Some(CacheEntry {
            version_tag: tag,
            callable: held,
            owner: owner.unbind(),
        }),
        _ => None,
    };
    let released = CACHE.with(|cache| {
        let mut cache = cache.borrow_mut();
        cache.misses += 1;
        let mut released = Vec::new();
        if let Some(entry) = entry {
            released.extend(cache.entries.insert(key, entry));
            if cache.entries.len() >= cache.next_sweep {
                released.extend(cache.sweep(py));
            }
        }
        released
    });
    drop(released);
    Ok(callable.unbind())
}

/// Whether the instance's own `__dict__` shadows `name` (rare path; falls
/// back to ordinary `call_method` to preserve exact attribute semantics).
fn instance_shadows(obj: &Bound<'_, PyAny>, name: &str) -> bool {
    let Ok(dict) = obj.getattr(intern!(obj.py(), "__dict__")) else {
        return false;
    };
    let Ok(dict) = dict.cast_into::<PyDict>() else {
        return false;
    };
    dict.contains(name).unwrap_or(false)
}

/// Call the cached instance method `obj.name()` with no arguments.
pub fn call_cached0<'py>(
    obj: &Bound<'py, PyAny>,
    name: &'static str,
) -> PyResult<Bound<'py, PyAny>> {
    if instance_shadows(obj, name) {
        return obj.call_method0(name);
    }
    let callable = resolve(&obj.get_type(), name)?;
    callable.bind(obj.py()).call1((obj,))
}

/// Call the cached instance method `obj.name(*args)`.
pub fn call_cached1<'py>(
    obj: &Bound<'py, PyAny>,
    name: &'static str,
    args: &Bound<'py, PyTuple>,
) -> PyResult<Bound<'py, PyAny>> {
    if instance_shadows(obj, name) {
        return obj.call_method1(name, args);
    }
    let callable = resolve(&obj.get_type(), name)?;
    let mut full = Vec::with_capacity(args.len() + 1);
    full.push(obj.clone());
    full.extend(args.iter());
    callable.bind(obj.py()).call1(PyTuple::new(obj.py(), full)?)
}

/// Call a cached `staticmethod` on `obj`'s type: `type(obj).name(*args)`.
/// Used for engine machinery functions (no `self`, no instance shadowing).
pub fn call_static_cached1<'py>(
    obj: &Bound<'py, PyAny>,
    name: &'static str,
    args: &Bound<'py, PyTuple>,
) -> PyResult<Bound<'py, PyAny>> {
    let callable = resolve(&obj.get_type(), name)?;
    callable.bind(obj.py()).call1(args)
}

/// Observer statistics for this thread's cache.
#[must_use]
pub fn stats() -> MethodCacheStats {
    CACHE.with(|cache| {
        let cache = cache.borrow();
        MethodCacheStats {
            entries: cache.entries.len() as u64,
            hits: cache.hits,
            misses: cache.misses,
            invalidations: cache.invalidations,
        }
    })
}

/// Zero the observer counters (entries stay cached; correctness never
/// depends on the counters).
pub fn reset_stats() {
    CACHE.with(|cache| {
        let mut cache = cache.borrow_mut();
        cache.hits = 0;
        cache.misses = 0;
        cache.invalidations = 0;
    });
}

/// Release this embedding worker's cached entries while its thread is still
/// attached: their weak references and native callables must not outlive the
/// isolated worker's module teardown.
pub(crate) fn clear_for_worker_teardown(_py: Python<'_>) {
    let entries = CACHE.with(|cache| std::mem::take(&mut cache.borrow_mut().entries));
    // A decref can invoke Python finalizers. Release the RefCell borrow before
    // dropping any callable or type so finalization cannot reenter that borrow.
    drop(entries);
}

/// `manimlib._method_cache_stats()`: deterministic cache observability.
#[pyfunction]
pub(crate) fn _method_cache_stats(py: Python<'_>) -> PyResult<Bound<'_, PyDict>> {
    let stats = stats();
    let out = PyDict::new(py);
    out.set_item("entries", stats.entries)?;
    out.set_item("hits", stats.hits)?;
    out.set_item("misses", stats.misses)?;
    out.set_item("invalidations", stats.invalidations)?;
    Ok(out)
}

/// `manimlib._method_cache_reset()`: zero the observer counters.
#[pyfunction]
pub(crate) fn _method_cache_reset() {
    reset_stats();
}

/// `(tag(cls), tag(type(cls)))`, the same ratified read the method cache
/// uses. A class's tag changes (or reads `0`) after any mutation of it or of
/// a base; the metaclass's tag covers the metaclass MRO. The Python
/// authored-override checks key their per-class verdicts on this pair
/// (fm-xte3): a verdict is reused only while both tags are unchanged and
/// nonzero.
#[pyfunction]
pub(crate) fn _type_version_tags(cls: &Bound<'_, PyType>) -> (u32, u32) {
    (type_version_tag(cls), type_version_tag(&cls.get_type()))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// fm-5wq.66: a dropped subclass, and what its methods close over, is
    /// collected after it used the cache; live classes still hit; dead
    /// entries are swept rather than accumulating.
    #[test]
    fn cache_entries_never_pin_dropped_classes() {
        crate::with_python_test_module("method cache lifetime", |py, _module, globals| {
            py.run(
                cr#"
import gc
import weakref
import manimlib as m

def local_subclass(captured):
    class Local(m.VGroup):
        def init_data(self, *args, **kwargs):
            captured.append(self)  # the class's method closes over its instance
            super().init_data(*args, **kwargs)
    Local()  # init_data/init_points/init_uniforms resolve through the cache
    return weakref.ref(Local)

captured = []
refs = [local_subclass(captured) for _ in range(3)]
instances = [weakref.ref(obj) for obj in captured]
captured.clear()
gc.collect(); gc.collect()
assert all(ref() is None for ref in refs), "a dropped subclass outlived its cached methods"
assert all(ref() is None for ref in instances), "an instance outlived its dropped subclass"

m._method_cache_reset()
m.VGroup(); m.VGroup()
stats = m._method_cache_stats()
assert stats["hits"] > 0, stats

for index in range(1500):
    local_subclass([])
    if index % 100 == 0:
        gc.collect()  # the classes are cyclic garbage (super()'s __class__ cell)
gc.collect()
m.VGroup()
assert m._method_cache_stats()["entries"] < 2200, m._method_cache_stats()
"#,
                Some(globals),
                Some(globals),
            )
            .inspect_err(|error| error.print(py))
            .expect("method cache lifetime");
        });
    }

    #[test]
    fn stats_start_zeroed() {
        reset_stats();
        let stats = stats();
        assert_eq!(stats.hits, 0);
        assert_eq!(stats.misses, 0);
        assert_eq!(stats.invalidations, 0);
    }
}
