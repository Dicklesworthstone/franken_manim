# BN-22: Bounded surface construction and first-error callback execution

**Status:** Draft.

## Problem and scope

The Python surface bridge delegates sampling and normals to Atlas. Its former
infallible native sampling loop stored the first Python error but kept invoking
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

## Native fallible sampling

`SurfaceSpec::try_sample` and `try_sample_with_budget` now perform admission and
sampling in Rust, returning `SurfaceSampleError<E>`. The callback may borrow its
environment and mutate local state; its error does not need `Clone`, `Send`, or a
`'static` lifetime. An error from a position or either derivative probe returns
immediately. No later probe, remaining grid iteration, or triangle generation
runs, and no partially sampled surface is returned. Successful sample ordering
and normal arithmetic remain unchanged.

Both dimensions, their checked product, all record columns, and triangle storage
are admitted before authored sampling. Every output allocation for the sampled
surface is reserved before the first callback. Samples and derived normal-control
points must be finite and f32-representable. Existing empty/strip behavior remains.
The explicit budget belongs to the native caller; the Python raw bridge uses the
default budget. The existing infallible `sample` entry delegates to this owner and
panics on a violated construction contract; untrusted native inputs should use
the fallible API.

The raw PyO3 parametric-surface method now calls the fallible native API, including
when deliberately invoked around the host admission adapter. It preserves the
original Python exception and extracts exactly three coordinates without
materializing an unbounded result vector. Scene-bound construction is refused
before sampling. The host adapter remains responsible for Python input conversion
and aggregate multi-face solid admission; it is not a second native sampler.

## Planar OBJ polygon admission

The owned OBJ reader formerly fan-triangulated every polygon. A concave face
could therefore cover an authored cutout or overlap itself. It now accepts
simple planar polygons through Chisel's boundary-preserving triangulator.
This is an intentional geometry correction: concave output may change
triangle order and visible coverage. Valid nondegenerate convex fans keep
their original order, and original three-corner statements retain their
direct behavior, including defined zero-area triangles.

The reader first normalizes finite 3D coordinates into bounded predicate
space, accumulates the face area vector in source order, and chooses its
largest absolute component with a fixed X/Y/Z tie-break. The cyclic projected
axis pairs retain the sign of that component; emitted triangles restore the
source face's 3D winding. Every accepted polygon remains within `1e-6` of the
largest bounding-box extent from the face plane through its first corner.
Original 3D coordinates are never flattened or otherwise repaired.

The narrow `triangulate_preserving_vertices` entry point shares Chisel's
normalization, boundary predicates and strict ear checks. It does not clean
collinear vertices, merge positions, split recursively, or discard a remnant.
Every accepted input index participates; the output contains exactly `n - 2`
nondegenerate triangles. This retains authored UV/normal values on straight boundary runs.
The existing general `triangulate` cleanup and hole behavior are unchanged.

Only an exactly repeated closing `(v, vt, vn)` tuple is redundant. Other
coincident, touching, crossing or backtracking polygon boundaries refuse.
The material layer consumes the geometry parser's actual source-face triangle
intervals; it no longer estimates face size from token count. A closing tuple
cannot shift subsequent material bindings.

Before face storage is collected, each source face is bounded to 4096 corners.
The triangulator's additional 67,108,864-unit work budget charges every
simplicity pair and reserves six full vertex scans per ear candidate, including
failed candidates. Budget exhaustion and geometric refusal include the source
face line. The total emitted triangle limit is checked before triangulation,
after removing an exactly repeated closing tuple.
No dependency, wire-format, renderer arithmetic or host file authority changes.

## Explicit limitations

The native fallible API applies to rectangular UV sampling, not every specialized
solid's post-sampling transformation or compound allocation. In particular, a
direct Rust cube/prism builder still does not expose a fallible aggregate
six-face budget. Python aggregate admission and native single-grid admission are
distinct contracts. The renderer's arithmetic domain is unchanged; finite inputs
alone do not prove that all later transforms remain representable. A callback
that does not return cannot be preempted by this synchronous error channel.

Authored side effects before a callback failure are not rolled back. Existing
empty/strip-grid restrictions in UV queries and morph alignment are unchanged.

## Regression evidence

`crates/fmn-python/tests/surface_admission.py` exercises real native construction,
stock-solid budgets, exception identity, probe ordering, owner preservation,
reentrancy, live regeneration, and successful record/pixel equivalence with the
unguarded native sampler on safe inputs. The surface workflow and installed-wheel
gate execute this suite. No giant unchecked allocation is used as a negative
control.

`crates/fmn-library/tests/surface_sampling.rs` verifies the native owner directly.
`crates/fmn-python/tests/native_surface_sampling.py` bypasses the host callback
guard and tests the raw bridge's admission, each derivative-probe failure,
original exception identity, and unchanged target records/views after refusal.

`crates/fmn-geom/src/earclip.rs` verifies the preserving entry point's area,
cutout coverage, collinear indices, numerical normalization, topology refusals,
and both simplicity/ear work accounting. `crates/fmn-library/tests/obj_polygons.rs`
checks all dominant planes and windings, negative indices, actual material
ranges, exact native UV/normal records, tight budgets, and source-line refusals.
The registered `render_matrix.obj_concave_faces.v1` scenario exercises native
textured model composition, independent cutout probes, durable restoration,
and camera rendering at 1/4/16 threads.
