# Owned Python data in SceneState

`Scene.get_state()`, undo/redo, and checkpoint restoration retain the native
record/clock/RNG checkpoint and the same original Python mobject identities.
They do not execute another scene or serialize callbacks.

The Python projection now freezes ordinary authored attributes and extension
uniform values in **one graph**, instead of maintaining a separate shallow
uniform-copy protocol. A list or array shared by a custom attribute, two
mobjects' extension uniforms, and the camera remains shared after restoration.
The restored values do not alias the live pre-restore data or the saved snapshot.
Repeated restoration therefore returns to the same saved values after edits.
Native uniform fields remain owned by the native checkpoint.

## Owned values and comparison

Plain dictionaries, lists, tuples, sets, bytearrays and NumPy arrays are copied.
Object-array elements follow the same policy: nested owned containers are
frozen, while opaque resources, native proxies and callbacks keep identity.
Cycles and repeated references are retained. Array subclasses are normalized
to plain arrays without invoking their authored `copy`/`deepcopy` methods.
Shapes, dtypes, data values and read-only array flags survive.

NumPy structured scalars (`np.void`) are mutable views, including records with
only numeric fields. They now receive independent storage and participate in
the alias memo before object fields are traversed. Self-referential record
slots no longer recurse until the structural limit. Separate overlapping NumPy
views are copied as separate owned values; this is not a reconstruction of the
original buffer/stride-sharing graph.

Change detection compares owned values and their alias topology. It does not
invoke opaque `__eq__` through NumPy object arrays. Signed zeros and NaN payload
bits are preserved and compared, avoiding both missed edits and repeated
no-op saves for an unchanged NaN. An unchanged live array subclass compares to
its normalized snapshot. Camera extension data participates even when the
camera is not one of the scene roots; an ignored camera remains untouched.

## Failure boundaries

The existing structural limits (262,144 visited nodes, 128 levels, and 64 MiB
of copied array/bytearray data) apply to the combined attributes/uniform graph.
A shared value is charged once. This is a Python data-buffer budget, not a
whole-process or native-checkpoint memory limit; comparison mirrors and native
snapshot storage also occupy memory.

Both Python projections are validated and allocated **before** invoking native
restoration. A malformed projection or a structural allocation-budget refusal
therefore cannot restore geometry/time and then fail while copying uniforms.
The existing history transition still occurs only after restoration succeeds.
This is not a new rollback guarantee for errors thrown inside the native
restorer or arbitrary authored scene hooks.

## Tests

`test_scene_uniform_ownership.py` uses real NumPy and the existing explicit
native-storage fixture to exercise production snapshot orchestration, aliasing,
cycles, exact values, repeated restore, history transitions and pre-restore
refusals. Its simulated checkpoint bytes are not native serialization evidence.
The unchanged structural and SceneState protocol suites remain regressions.

`test_native_uniform_checkpoint.py` separately exercises actual native Scene
and mobject instances. Run with `FMN_TEST_NATIVE=1` and the matching installed
wheel so a missing native build is an error rather than a skipped acceptance.
