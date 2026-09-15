# Persistent native animations

The wheel's animation-to-updater adapter can retain Choreo's existing native
animation driver for a scene-bound animation with no Python interpolation
implementation. `Swap` and `CyclicReplace` no longer stop at the previous
native-driver refusal. No new renderer, geometry kernel, segment or clock is
introduced: the Scene updater's `dt` drives the retained leaf lifecycle.

```python
scene.add(left, right)
cycle_animation(Swap(left, right, path_arc=0, run_time=2, rate_func=linear))
scene.wait(5)
left.clear_updaters()
```

The helper preserves its pre-increment elapsed-time convention and its
interpolate-then-helper-update order. Noncycling effects finish once; cycling
ones wrap the existing alpha without repeatedly allocating native drivers.
Native finishing is distinct from scene cleanup: persistent effects do not
remove drawable roots or publish replacement targets. Targets and additional
participants are adopted by the existing lowering path, not added to the draw
list. With the current bridge's multi-object native specifications, the first
mobject remains the returned updater anchor; add the other participants to the
Scene as in the example.

Removing or clearing the installed updater aborts its retained driver without
jumping to the final pose. Clearing a copied updater does not cancel the original
controller. Copies and reentrant helper updates cannot double-advance its time.
Failures detach only the owned updater, release native animation state, and
preserve the original exception when cleanup also fails. Mobjects from different
Scenes are rejected before driver lowering.

Existing callback implementations remain the preferred route, preserving
live authored hooks and endpoint options. Native-only animations use the same
specification lowering as Scene.play. Their configuration is frozen when the
native driver is constructed; custom rates retain that lowering's current
sample-table semantics. Native-only nondefault final alpha and authored lifecycle
hooks without a Python interpolator are refused rather than silently discarded.
Detached native-only animations still require scene adoption before registration.

The installed wheel installs these helpers through the existing playback
adapter. The independent embedded initialization and native Rust API are
unchanged. Existing function aliases in compatibility modules are refreshed;
public Animation class objects retain their identities.

`test_native_animation_updater_protocol.py` extracts the production driver
factory and native-leaf wrapper, substitutes native storage/execution, and
exercises the real persistent controller. It is not native geometry or rendering
acceptance. The installed-wheel gate registers six additional real-extension
cases in `native_animation_updaters.py`. Execution requires a built extension;
no native-rendering, certification or performance claim follows from protocol
results.
