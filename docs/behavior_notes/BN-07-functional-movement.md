# BN-07: Functional movement lifecycle and flow replay

**Status:** Draft.

**Scope:** the optional Python wheel's `Homotopy`,
`SmoothedVectorizedHomotopy`, `ComplexHomotopy`, `PhaseFlow`, and
`MoveAlongPath`, plus the native Rust vector-field updater helpers. Related
contract: plan sections 7.3, 8.6, 9.1–9.4, 12.4 and 15.2; task
`fm-5wq.4.143`.

## Family-aware deformation

Homotopy uses the shared Animation lifecycle: the starting-copy hook, family
zipper, time-span normalization, per-member lag and easing, helper updates,
and final-alpha interpolation. This corrects the old portal shortcut that
copied directly, skipped point-free family roots and sent every pointful
member the same alpha. Authored `create_starting_mobject`,
`get_all_families_zipped`, `get_sub_alpha`, and `function_at_time_t` hooks run.
The existing mobject methods still own point restoration, mapping and native
smoothing; complex homotopy continues to carry the third coordinate unchanged.

For example, inside a Scene:

```python
self.play(Homotopy(
    lambda x, y, z, t: (x, y + t * x * x, z),
    curves,
    lag_ratio=0.1,
    run_time=2,
    suspend_mobject_updating=True,
))
```

Geometry is reconstructed from the starting copy at each sample, so an
absolute deformation can be sampled backward without accumulating prior
maps. This is not a claim that an arbitrary authored callback is pure or
eligible for frame-parallel execution.

## Native vector-field advection

`move_points_along_vector_field` advances every point-bearing member of the
receiver's family in world coordinates, including children of a point-less
group. Retained translations, rotations and scales are respected before the
coordinate system converts points and velocities. A shared descendant is
mapped once. Auxiliary pointlike records, including surface normal control
points, use the same mapping; invalid surface samples remain holes and are
not passed to the field callback.

The point update is
`p + (c2p(function(p2c(p))) - origin) * dt`, using the supplied coordinate
system's units. It retains the existing per-tick forward-Euler semantics.

All three native helpers—`move_along_vector_field`,
`move_submobjects_along_vector_field`, and
`move_points_along_vector_field`—register fallible updaters. Invalid time
deltas, non-finite callback results and unrepresentable mapped geometry
produce the existing typed updater failure, which scene playback propagates.
Every affected family is checked before publication, so a bad later sample
cannot partially advance earlier members. Submobject motion retains its
sequential semantics when children share descendants, including the f32
baking required by attached record views. Authored callback side effects
outside the geometry are not rolled back.

The checked operations are also available directly as
`Stage::try_apply_points_function` and `Stage::try_shift`. The existing
infallible methods retain their API.

Regression coverage lives in `fmn-library/src/fields.rs` and
`fmn-mobject/tests/pointlike_transforms.rs`. The
`render_matrix.native_field_advection.v1` scenario compares a moving, placed
group against independently positioned native geometry at 1, 4 and 16 workers.

## PhaseFlow restart and explicit zero

Two deliberate corrections bring Python flow into agreement with the existing
Rust `PhaseFlow::new` and `PhaseFlow::setup` in `fmn-anim/src/movement.rs`:

* `virtual_time=None` adopts the construction-time runtime; **explicit zero
  stays zero** rather than falling through the Reference's `or run_time`.
* Every `begin()` resets the previous alpha. Reusing a flow starts from the
  current live geometry, not an unintended negative-time step from the last
  run's final alpha to the new run's alpha zero.

Migration: code needing the default duration should omit `virtual_time` or
pass `None`, not `0`. Code intentionally reversing a flow should request a
negative finite virtual time or explicitly sample backward within an active
run; restarting an animation is no longer an implicit reversal.

The existing **stateful forward-Euler** update remains
`p += virtual_time * (alpha - last_alpha) * function(p)`. It uses raw alpha,
as the pinned Reference and native kernel do; lag, rate functions and time
spans are not inserted into that integration formula. This is not an RK
integrator, a constant-accuracy solver, or a frame-rate-independent flow.

## Live path-motion hooks

Unmodified MoveAlongPath retains its native route and Chisel's true-arclength
sampler (BN-03). The native route already reads live path geometry; this
change does not replace a stale-path cache.

Authored path samplers, moving-object methods, animation lifecycle overrides,
custom leaf rates, nondefault final alpha and removers select the existing
callback route. Instance replacements and changed base methods reached through
`super()` are detected. Static inspection does not execute user descriptors
merely to choose a route. On the callback route the existing
`path.point_from_proportion(rate_func(alpha))` and `mobject.move_to(point)`
execute at each actual interpolation request, not at guessed probe points.

For a wholly callback-driven play containing a top-level path animation,
a custom play-level rate is assigned before lowering and the redundant global
sampled payload is omitted. Plays with native siblings keep the original global
rate payload and sampling density; group rates remain group rates. This does
**not** change global custom-rate behavior for plays without a top-level path
animation or promise that native sibling/group curves never use their existing
sampling mechanism.
MoveAlongPath continues to rate raw alpha without leaf lag/time-window
remapping, matching its existing native convention.

## Failure ownership and installation

These movement callbacks acquire suspension per family member. Finish restores
only suspension acquired by the animation and retains the normal zero-dt
resume update. Abort does not run another updater. Failures in maps, path
samplers, helper callbacks, sibling animations or scene updates unwind active
movement callbacks while preserving the original exception; cleanup errors
are attached as notes. This is resource/lifecycle cleanup, **not a transaction
rolling back geometry already changed by authored code**. Previously suspended
children stay suspended. Unbegun or aborted remover animations do not remove
objects during cleanup.

The wheel's existing playback installer invokes `install_movement` on the
same public class table. Qualified class identities and subclass relations
are retained; reinstallation does not overwrite later authored replacements.
Direct extension consumers bypassing the package initializer must explicitly
install the adapter. This does not alter the independent embedded Rust module
initialization or imply complete portal parity. Callback-selected paths may
add interpreter overhead, which has not been benchmarked.

## Validation boundary

`test_homotopy_protocol.py`, `test_flow_path_protocol.py`,
`test_movement_frontend.py` and `test_movement_initializer.py` exercise
production definitions with fixture object storage and a native execution
boundary double. The frontend suite uses the actual `Scene.play` definition,
including its specification lowering and rate-payload construction. These
checks are not native-rendering acceptance.

The installed-wheel runner includes `movement_semantics.py`: fifteen
real-extension cases cover deformation, hooks, timing, suspension, flow reuse,
path motion, mixed native/callback succession and exception recovery. A Y4M
case independently decodes luma and checks the moving shape's pixel centroid,
so it tests renderer consumption, not just a getter. Artifacts are retained
for inspection. Running the registered suite requires a built extension;
source protocol success does not substitute for that execution, the complete
Rust workspace gate, performance qualification or certification.
