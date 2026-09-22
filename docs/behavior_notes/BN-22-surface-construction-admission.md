# BN-22: Bounded surface construction and first-error callback execution

## Problem and scope

The Python surface bridge delegates sampling and normals to Atlas. Its current
infallible native sampling loop stores the first Python error but keeps invoking
the callback: a 2-by-2 surface could run a failing user function twelve times.
Native stock surface constructors also accepted sizes without an authoring budget.
The existing plotting-only guard did not cover Surface subclasses, stock solids,
or direct calls to the normal Python object's native construction methods.

The production portal now admits those calls through one surface-construction
adapter. It does not implement another geometry sampler or change the native
sample ordering, normal calculation, storage, or renderer.

## Behavior

* A construction may invoke authored code only through its first failed sample.
  The original exception, including KeyboardInterrupt or SystemExit, propagates.
  Result conversion errors receive the same treatment. Each construction has its
  own guard; a nested failed construction does not poison an outer one.
* UV functions return exactly three finite real, f32-representable coordinates.
  Extra components, non-real arrays, NaN, infinity and out-of-record-range samples
  are refused rather than truncated or stored as invalid native points.
* Grid axes are nonnegative integer indices. Floating-point dimensions are not
  silently truncated. Each axis and the total grid are bounded by 65,536. The
  per-axis bound also protects empty grids such as `(0, very_large)`.
* Cube construction charges all six sampled faces against the same aggregate
  point budget. Ordinary small cube/prism defaults are unchanged.
* UV ranges and derivative controls must be finite. Epsilon must be positive,
  normal nudge nonnegative, and preferred creation axis either zero or one.
  Reversed and collapsed finite ranges, empty grids and one-point-wide strips
  keep their existing construction behavior.
* A scene-bound object cannot be reconstructed with `__init__` or a construction
  method. Use `init_points()` for supported live regeneration, or `become()` for
  replacement. Rejected calls do not reset the object's scene mirror or invoke
  the UV function. A failed detached native build does not replace its geometry.

Successful constructor signatures and authored function identities are retained.
The adapter is installed by the one production initializer, before plotting uses
its shared admission rules. Saved native methods are resolved from the native
namespace rather than held in class-method closures, following the portal's
module-lifetime protocol.

## Explicit limitations

This protects the public Python constructors and the ordinary Mobject method
resolution path, including their underscored native builder seams. It does not
change direct Rust builders, raw PyO3 descriptors deliberately called around the
adapter, or the renderer's arithmetic domain. Finite inputs alone do not prove
that every possible derived point or normal from an extreme solid is finite.

The legacy native sampler can still finish its bounded remaining internal
iterations after a failure. The guard returns harmless sentinel samples during
that unwinding, without calling authored code or repeatedly growing a traceback.
The native first-error channel then refuses result publication. Replacing this
with a fallible native sampler is separate work; this change does not claim to
complete that Rust-level allocation or immediate-termination contract.

Authored side effects before a callback failure are not rolled back. Existing
empty/strip-grid restrictions in UV queries and morph alignment are unchanged.

## Regression evidence

`crates/fmn-python/tests/surface_admission.py` exercises real native construction,
stock-solid budgets, exception identity, probe ordering, owner preservation,
reentrancy, live regeneration, and successful record/pixel equivalence with the
unguarded native sampler on safe inputs. The surface workflow and installed-wheel
gate execute this suite. No giant unchecked allocation is used as a negative
control.
