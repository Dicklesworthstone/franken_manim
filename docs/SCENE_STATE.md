# Scene snapshots and undo/redo

The installed `manimlib` package installs `fmn_python.scene_state` on the
existing `SceneState` and `Scene` classes. Qualified imports keep the same
class objects. An embedding that loads the extension directly may explicitly
call `install_scene_state(native)` after initializing its module.

```python
from manimlib import Scene, Square, Group, RIGHT

scene = Scene()
square = Square(fill_opacity=1)
scene.add(Group(square))
scene.save_state()
square.shift(2 * RIGHT)
scene.undo()
scene.redo()
```

## Captured state

For an ordinary full snapshot, Proscenium's existing native checkpoint remains
the authority for records, draw roots, scene time, and native RNG state. The
Python layer retains original descendant proxies, ordered family edges,
Python updater identities, extension uniforms, and public data-lock sets.
Mutable numeric arrays and built-in containers in extension uniforms are
copied rather than left as shallow aliases to the live scene.

The separate camera frame is also captured: center, shape, quaternion, field
of view, Euler axes, default orientation, and Python updater list. Restoration
uses the native camera setters on the original core, not a replacement handle.
A snapshot does not capture all camera configuration, light/background state,
or arbitrary user-defined object attributes.

Snapshot reuse compares complete ordered families, structured records,
uniforms, updater lists, and graph identities. Point-free groups are not
assumed to be unchanged. Root reordering and replacing a child with an
identically drawn but different object count as state changes. Exact record
comparison is intentional: small edits must not disappear through a tolerance.
`n_changes` compares the captured object mirrors, not their subsequently edited
live sources; like the Reference, its root count is directional. A changed
camera contributes one additional change.

## History behavior

`save_state` does not append an unchanged state. A new save clears the old
redo branch in place. `max_num_saved_states` bounds retained history entries;
zero disables saves and clears history. Negative and non-integer values are
rejected. Existing `get_state` and `restore_state` overrides remain active.

Undo/redo update their stacks only after `restore_state` returns successfully.
A failing native restore therefore leaves the history entries available for
inspection or retry. This is not transactional rollback of arbitrary authored
Python code: a custom restore hook that mutates objects and then raises may
have left those mutations in the scene even though the stacks are unchanged.

## Boundaries

A state belongs to its originating Scene. Cross-scene restoration is refused
before native or Python state mutation; copy objects explicitly to transfer
content. Native checkpoint decoding and stale-handle validation remain native.

Callbacks and opaque host resources are retained by identity. Their closure
variables, generators, external files, Python-global RNGs, and already-running
persistent-animation controllers are not rewound. Neither are bytes already
emitted to an active output sink; restoring scene time is not media rollback.

`SceneState(scene, ignore=[scene.frame])` leaves the current camera pose alone
while retaining the full native checkpoint when all drawable roots are still
included. Excluding a drawable root continues to use the existing partial
`become`/root replacement path. That partial path does not promise native
clock/RNG rewind or preservation of all original descendant identities.

## Execution coverage

`test_scene_state_protocol.py` and `test_scene_history_protocol.py` exercise
orchestration against explicitly doubled storage. They do not prove native
checkpoint behavior. The installed-wheel gate additionally registers
`tests/scene_state.py`, which requires actual native objects, checkpoint bytes,
repeated history restoration, and decoded Y4M output. Native acceptance must
be executed against a wheel built from the same checkout before claiming that
integration verified; no compatibility or performance gate is waived here.
